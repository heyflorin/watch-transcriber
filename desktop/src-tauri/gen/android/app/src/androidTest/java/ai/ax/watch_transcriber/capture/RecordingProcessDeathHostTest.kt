package ai.ax.watch_transcriber.capture

import android.Manifest
import android.app.ActivityManager
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Environment
import android.os.SystemClock
import android.provider.MediaStore
import androidx.core.content.ContextCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.nio.file.Files
import java.security.MessageDigest
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.json.JSONObject

/**
 * Host-orchestrated process-death proof.
 *
 * Run each method in a separate `am instrument` invocation and issue
 * `am force-stop ai.ax.watch_transcriber` between them. The Gradle connected
 * suite excludes this class because executing both phases in one process would
 * not test the contract and JUnit method order is intentionally unspecified.
 */
@RunWith(AndroidJUnit4::class)
class RecordingProcessDeathHostTest {
  @Test
  fun physicalTaskRemovalLeavesForegroundRecorderAlive() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    check(
      context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) ==
        android.content.pm.PackageManager.PERMISSION_GRANTED,
    ) { "physical task-removal proof requires pre-granted microphone permission" }
    val captureRoot = File(context.filesDir, "capture")
    check(!captureRoot.exists() || captureRoot.walkTopDown().none(File::isFile)) {
      "physical task-removal proof requires an empty capture root"
    }
    val ready = File(context.filesDir, "physical-task-removal-ready")
    val survived = File(context.filesDir, "physical-task-removal-survived")
    check(!ready.exists() && !survived.exists()) {
      "physical task-removal proof markers already exist"
    }
    val sessionId = UUID.randomUUID().toString()
    val launch = requireNotNull(context.packageManager.getLaunchIntentForPackage(context.packageName))
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(launch)
    instrumentation.waitForIdleSync()
    ContextCompat.startForegroundService(
      context,
      Intent(context, RecordingService::class.java)
        .setAction(RecordingService.ACTION_START)
        .putExtra(RecordingService.EXTRA_SESSION_ID, sessionId),
    )
    awaitState(context, SessionState.RECORDING)
    val segment = File(captureRoot, "sessions/$sessionId/segment-0000.wav")
    awaitFileGrowth(segment)
    ready.writeText(sessionId, Charsets.UTF_8)

    val baseline = segment.length()
    val tasks = context.getSystemService(ActivityManager::class.java).appTasks
    check(tasks.isNotEmpty()) { "physical task-removal proof found no EchoWall task" }
    tasks.forEach { it.finishAndRemoveTask() }
    awaitFileGrowthBeyond(segment, baseline)
    check(SessionJournal.status(context).getString("state") == SessionState.RECORDING.name)
    survived.writeText(sessionId, Charsets.UTF_8)
    while (true) SystemClock.sleep(1_000)
  }

  @Test
  fun cleanupPhysicalTaskRemovalFixture() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    val ready = File(context.filesDir, "physical-task-removal-ready")
    val survived = File(context.filesDir, "physical-task-removal-survived")
    val marker = listOf(survived, ready).firstOrNull(File::isFile)
      ?: error("physical task-removal cleanup marker is missing")
    check(!Files.isSymbolicLink(marker.toPath()))
    val sessionId = marker.readText(Charsets.UTF_8)
    check(sessionId.matches(Regex("[0-9a-f-]{36}")))
    val captureRoot = File(context.filesDir, "capture")
    val sessions = File(captureRoot, "sessions")
    val session = File(sessions, sessionId)
    check(session.isDirectory && !Files.isSymbolicLink(session.toPath()))
    val sessionEntries = session.listFiles().orEmpty()
    check(sessionEntries.isNotEmpty())
    check(sessionEntries.all { file ->
      file.isFile && !Files.isSymbolicLink(file.toPath()) &&
        file.name.matches(Regex("segment-[0-9]{4}\\.wav"))
    })
    val rootEntries = captureRoot.listFiles().orEmpty()
    check(rootEntries.all { entry ->
      entry.name in setOf("sessions", "active-session.json", "session-events.ndjson") &&
        !Files.isSymbolicLink(entry.toPath())
    })
    val sessionDirectories = sessions.listFiles().orEmpty()
    check(sessionDirectories.size == 1 && sessionDirectories[0].name == sessionId)

    sessionEntries.forEach { check(it.delete()) }
    check(session.delete())
    check(sessions.delete())
    rootEntries.filter { it.name != "sessions" }.forEach { check(it.delete()) }
    check(captureRoot.delete())
    if (ready.exists()) check(ready.delete())
    if (survived.exists()) check(survived.delete())
    check(!captureRoot.exists() && !ready.exists() && !survived.exists())
  }

  @Test
  fun phaseOneLeavesActiveRecordingForHostKill() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    grant(context, Manifest.permission.RECORD_AUDIO)
    if (Build.VERSION.SDK_INT >= 33) grant(context, Manifest.permission.POST_NOTIFICATIONS)
    val appRoot = requireNotNull(context.filesDir.parentFile)
    val data = File(appRoot, "data")
    check(data.mkdirs() || data.isDirectory)
    File(data, "manifest.json").writeText("{}", Charsets.UTF_8)
    val captureRoot = File(context.filesDir, "capture")
    captureRoot.deleteRecursively()
    val sessionId = UUID.randomUUID().toString()
    val launch = requireNotNull(context.packageManager.getLaunchIntentForPackage(context.packageName))
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(launch)
    instrumentation.waitForIdleSync()

    ContextCompat.startForegroundService(
      context,
      Intent(context, RecordingService::class.java)
        .setAction(RecordingService.ACTION_START)
        .putExtra(RecordingService.EXTRA_SESSION_ID, sessionId),
    )
    awaitState(context, SessionState.RECORDING)
    val segment = File(captureRoot, "sessions/$sessionId/segment-0000.wav")
    awaitFileGrowth(segment)
    assertTrue(RecordingService.isActive())
    assertTrue(segment.length() > WavFiles.HEADER_BYTES)
    File(captureRoot, "host-kill-ready").writeText(sessionId, Charsets.UTF_8)
    // Intentionally keep instrumentation and the recorder alive. The host
    // confirms the marker, PID, and durable snapshot, then kills the entire
    // target package. Reaching the line after this loop would invalidate the
    // proof, so the host cleanup trap is also a bounded escape hatch.
    while (true) SystemClock.sleep(1_000)
  }

  @Test
  fun phaseTwoRecoversRecordingInRecreatedProcess() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    val captureRoot = File(context.filesDir, "capture")
    val before = SessionJournal.status(context)
    val sessionId = before.getString("sessionId")
    val segment = File(captureRoot, "sessions/$sessionId/segment-0000.wav")

    assertFalse(RecordingService.isActive())
    assertEquals(SessionState.RECORDING.name, before.getString("state"))
    assertTrue(segment.isFile)
    assertTrue(segment.length() > WavFiles.HEADER_BYTES)

    val launch = requireNotNull(context.packageManager.getLaunchIntentForPackage(context.packageName))
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(launch)
    InstrumentationRegistry.getInstrumentation().waitForIdleSync()
    awaitState(context, SessionState.INTERRUPTED)
    val recovered = SessionJournal.status(context)
    assertEquals(SessionState.INTERRUPTED.name, recovered.getString("state"))
    val expectedPcmBytes = segment.length() - WavFiles.HEADER_BYTES
    assertEquals(
      expectedPcmBytes,
      WavFiles.repair(segment, RecordingService.SAMPLE_RATE, 1, 16),
    )
    val events = File(captureRoot, "session-events.ndjson").readText(Charsets.UTF_8)
    assertTrue(events.contains("\"kind\":\"interrupted\""))
    File(captureRoot, "host-recovery-ready").writeText(sessionId, Charsets.UTF_8)
    while (true) SystemClock.sleep(1_000)
  }

  @Test
  fun nativeImportStagedForHostKill() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    val appRoot = requireNotNull(context.filesDir.parentFile)
    val data = File(appRoot, "data")
    check(data.mkdirs() || data.isDirectory)
    File(data, "manifest.json").writeText("{}", Charsets.UTF_8)
    val source = File(context.cacheDir, "echowall-wrapper-${UUID.randomUUID()}.wav")
    WavFiles.initialize(source, RecordingService.SAMPLE_RATE, 1, 16)
    source.appendBytes(ByteArray(32_000) { index -> (index % 109).toByte() })
    WavFiles.repair(source, RecordingService.SAMPLE_RATE, 1, 16)
    val bytes = source.readBytes()
    val sha256 = MessageDigest.getInstance("SHA-256")
      .digest(bytes)
      .joinToString("") { "%02x".format(it) }
    val displayName = "EchoWall Rust Wrapper ${UUID.randomUUID()}.wav"
    val values = ContentValues().apply {
      put(MediaStore.Downloads.DISPLAY_NAME, displayName)
      put(MediaStore.Downloads.MIME_TYPE, "audio/wav")
      put(
        MediaStore.Downloads.RELATIVE_PATH,
        "${Environment.DIRECTORY_DOWNLOADS}/EchoWallIntegration",
      )
      put(MediaStore.Downloads.IS_PENDING, 1)
    }
    val resolver = context.contentResolver
    val uri = requireNotNull(
      resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values),
    )

    try {
      requireNotNull(resolver.openOutputStream(uri, "w")).use { it.write(bytes) }
      resolver.update(
        uri,
        ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) },
        null,
        null,
      )
      val staged = ImportInbox.copyPickedUri(context, uri)
      val nativeCopy = File(context.filesDir, staged.getString("relativePath"))
      assertTrue(nativeCopy.isFile)
      resolver.delete(uri, null, null)
      File(context.filesDir, "host-import-staged").writeText(
        JSONObject()
          .put("importId", staged.getString("importId"))
          .put("displayName", displayName)
          .put("sha256", sha256)
          .toString(),
        Charsets.UTF_8,
      )
      while (true) SystemClock.sleep(1_000)
    } finally {
      resolver.delete(uri, null, null)
    }
  }

  @Test
  fun nativeImportReachesRustInboxAfterProcessDeath() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    val appRoot = requireNotNull(context.filesDir.parentFile)
    val proof = JSONObject(
      File(context.filesDir, "host-import-staged").readText(Charsets.UTF_8),
    )
    val importId = proof.getString("importId")
    val displayName = proof.getString("displayName")
    val sha256 = proof.getString("sha256")
    val nativeCopy = File(context.filesDir, "inbox/$importId.wav")
    assertTrue(nativeCopy.isFile)

    val launch = requireNotNull(context.packageManager.getLaunchIntentForPackage(context.packageName))
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(launch)
    instrumentation.waitForIdleSync()
    val envelopeFile = awaitRustAdoption(appRoot, displayName, nativeCopy)
    val envelope = JSONObject(envelopeFile.readText(Charsets.UTF_8))
    assertEquals(displayName, envelope.getString("imported_name"))
    assertEquals("android", envelope.getJSONObject("source").getString("platform"))
    assertEquals("ready", envelope.getJSONObject("job").getString("state"))
    assertEquals(sha256, envelope.getString("normalized_sha256"))
    val normalized = File(
      requireNotNull(envelopeFile.parentFile),
      envelope.getString("normalized_audio"),
    )
    assertTrue(normalized.isFile)
    assertEquals(sha256, MessageDigest.getInstance("SHA-256")
      .digest(normalized.readBytes())
      .joinToString("") { "%02x".format(it) })
    assertFalse(nativeCopy.exists())
    File(context.filesDir, "host-import-ready").writeText(
      requireNotNull(envelopeFile.parentFile).name,
      Charsets.UTF_8,
    )
    while (true) SystemClock.sleep(1_000)
  }

  private fun grant(context: Context, permission: String) {
    shell("pm grant ${context.packageName} $permission")
  }

  private fun shell(command: String) {
    InstrumentationRegistry.getInstrumentation().uiAutomation
      .executeShellCommand(command)
      .close()
  }

  private fun awaitState(context: Context, expected: SessionState) {
    val deadline = SystemClock.uptimeMillis() + 10_000
    while (SystemClock.uptimeMillis() < deadline) {
      if (SessionJournal.status(context).optString("state") == expected.name) return
      SystemClock.sleep(50)
    }
    error("recording service did not reach ${expected.name}")
  }

  private fun awaitFileGrowth(file: File) {
    val deadline = SystemClock.uptimeMillis() + 10_000
    while (SystemClock.uptimeMillis() < deadline) {
      if (file.isFile && file.length() > WavFiles.HEADER_BYTES) return
      SystemClock.sleep(50)
    }
    error("recording service did not write PCM data")
  }

  private fun awaitFileGrowthBeyond(file: File, baseline: Long) {
    val deadline = SystemClock.uptimeMillis() + 10_000
    while (SystemClock.uptimeMillis() < deadline) {
      if (file.isFile && file.length() > baseline) return
      SystemClock.sleep(50)
    }
    error("recording service stopped writing after physical task removal")
  }

  private fun awaitRustAdoption(appRoot: File, displayName: String, nativeCopy: File): File {
    val rustInbox = File(appRoot, "inbox")
    val deadline = SystemClock.uptimeMillis() + 20_000
    while (SystemClock.uptimeMillis() < deadline) {
      val envelope = rustInbox.listFiles()
        ?.asSequence()
        ?.map { File(it, "recording.json") }
        ?.filter(File::isFile)
        ?.firstOrNull { file ->
          try {
            JSONObject(file.readText(Charsets.UTF_8)).optString("imported_name") == displayName
          } catch (_: Exception) {
            false
          }
        }
      if (envelope != null && !nativeCopy.exists()) return envelope
      SystemClock.sleep(100)
    }
    error("native import did not reach the Rust inbox and acknowledgement boundary")
  }
}
