package ai.ax.watch_transcriber.capture

import android.Manifest
import android.app.ActivityManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.SystemClock
import androidx.core.content.ContextCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RecordingServiceInstrumentedTest {
  @Test
  fun foregroundRecorderPersistsPauseResumeAndStoppedWav() {
    assumeTrue(
      "screen-off instrumentation is emulator-only; physical devices use the host lifecycle harness",
      isEmulatorBuild(),
    )
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    grant(context, Manifest.permission.RECORD_AUDIO)
    if (Build.VERSION.SDK_INT >= 33) grant(context, Manifest.permission.POST_NOTIFICATIONS)
    val captureRoot = File(context.filesDir, "capture")
    captureRoot.deleteRecursively()
    val sessionId = UUID.randomUUID().toString()
    val launch = requireNotNull(context.packageManager.getLaunchIntentForPackage(context.packageName))
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(launch)
    instrumentation.waitForIdleSync()

    try {
      ContextCompat.startForegroundService(
        context,
        Intent(context, RecordingService::class.java)
          .setAction(RecordingService.ACTION_START)
          .putExtra(RecordingService.EXTRA_SESSION_ID, sessionId),
      )
      awaitState(context, SessionState.RECORDING)
      val segment = File(captureRoot, "sessions/$sessionId/segment-0000.wav")
      awaitFileGrowth(segment)
      assertTrue(RecordingService.latestLevel() in 0f..1f)

      val beforeTaskRemoval = segment.length()
      val tasks = context.getSystemService(ActivityManager::class.java).appTasks
      assertTrue(tasks.isNotEmpty())
      tasks.forEach { it.finishAndRemoveTask() }
      awaitFileGrowthBeyond(segment, beforeTaskRemoval)
      assertEquals(SessionState.RECORDING.name, SessionJournal.status(context).getString("state"))

      val beforeScreenOff = segment.length()
      shell("input keyevent 223")
      awaitFileGrowthBeyond(segment, beforeScreenOff)
      assertEquals(SessionState.RECORDING.name, SessionJournal.status(context).getString("state"))
      shell("input keyevent 224")

      context.startService(
        Intent(context, RecordingService::class.java).setAction(RecordingService.ACTION_PAUSE),
      )
      awaitState(context, SessionState.PAUSED)
      assertEquals(0f, RecordingService.latestLevel())
      context.startService(
        Intent(context, RecordingService::class.java).setAction(RecordingService.ACTION_RESUME),
      )
      awaitState(context, SessionState.RECORDING)
      context.startService(
        Intent(context, RecordingService::class.java).setAction(RecordingService.ACTION_STOP),
      )
      awaitState(context, SessionState.STOPPED)
      assertEquals(0f, RecordingService.latestLevel())

      assertTrue(segment.isFile)
      assertTrue(segment.length() > WavFiles.HEADER_BYTES)
      assertEquals(segment.length() - WavFiles.HEADER_BYTES, WavFiles.repair(
        segment,
        RecordingService.SAMPLE_RATE,
        1,
        16,
      ))
      val events = File(captureRoot, "session-events.ndjson").readText(Charsets.UTF_8)
      assertTrue(events.contains("\"kind\":\"paused\""))
      assertTrue(events.contains("\"kind\":\"recording\""))
      assertTrue(events.contains("\"kind\":\"stopped\""))
    } finally {
      shell("input keyevent 224")
      context.stopService(Intent(context, RecordingService::class.java))
      captureRoot.deleteRecursively()
    }
  }

  @Test
  fun staleRecordingSnapshotBecomesInterruptedAndRepairsWav() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    val captureRoot = File(context.filesDir, "capture")
    captureRoot.deleteRecursively()
    val journal = SessionJournal(context)
    val sessionId = UUID.randomUUID().toString()
    val session = journal.begin(sessionId)
    val segment = File(session, "segment-0000.wav")
    WavFiles.initialize(segment, RecordingService.SAMPLE_RATE, 1, 16)
    segment.appendBytes(ByteArray(32_000) { index -> (index % 127).toByte() })
    journal.checkpoint(0, 32_000)

    try {
      val recovered = journal.recoverInterrupted()
      assertEquals(SessionState.INTERRUPTED.name, recovered.getString("state"))
      assertEquals(32_000L, WavFiles.repair(segment, RecordingService.SAMPLE_RATE, 1, 16))
      val events = File(captureRoot, "session-events.ndjson").readText(Charsets.UTF_8)
      assertTrue(events.contains("\"kind\":\"interrupted\""))
    } finally {
      captureRoot.deleteRecursively()
    }
  }

  private fun grant(context: Context, permission: String) {
    shell("pm grant ${context.packageName} $permission")
  }

  private fun isEmulatorBuild(): Boolean =
    Build.FINGERPRINT.startsWith("generic") ||
      Build.FINGERPRINT.contains("emulator", ignoreCase = true) ||
      Build.MODEL.contains("Emulator", ignoreCase = true) ||
      Build.PRODUCT.contains("sdk", ignoreCase = true)

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
    error("recording service stopped writing PCM data")
  }
}
