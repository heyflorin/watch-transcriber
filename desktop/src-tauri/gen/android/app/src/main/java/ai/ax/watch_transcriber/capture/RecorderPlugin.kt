package ai.ax.watch_transcriber.capture

import android.Manifest
import android.app.Activity
import android.content.ComponentName
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.StatFs
import androidx.activity.result.ActivityResult
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.Permission
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.UUID

@TauriPlugin(
  permissions = [
    Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "recordAudio"),
    Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications"),
  ],
)
class RecorderPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun permissionStatus(invoke: Invoke) {
    invoke.resolve(permissionPayload())
  }

  @Command
  fun preflight(invoke: Invoke) {
    val result = permissionPayload()
    val native = currentStatus()
    val nativeState = native.optString("state", SessionState.IDLE.name)
    val availableBytes = availableStorageBytes()
    result.put("activityVisible", activity.hasWindowFocus())
    result.put("serviceDeclared", serviceDeclared())
    result.put("pendingRecovery", nativeState != SessionState.IDLE.name)
    result.put("storageAvailableBytes", availableBytes)
    result.put("storageReady", CaptureStorage.isReady(availableBytes))
    result.put(
      "canStart",
      result.getBoolean("recordAudio") &&
        result.getBoolean("notifications") &&
        result.getBoolean("activityVisible") &&
        result.getBoolean("serviceDeclared") &&
        result.getBoolean("storageReady") &&
        nativeState == SessionState.IDLE.name,
    )
    result.put("captureScope", "microphone")
    result.put("systemAudioSupported", false)
    invoke.resolve(result)
  }

  @Command
  fun start(invoke: Invoke) {
    val permissions = permissionPayload()
    if (!permissions.getBoolean("recordAudio") || !permissions.getBoolean("notifications")) {
      invoke.reject("recording permissions are not granted")
      return
    }
    if (!activity.hasWindowFocus()) {
      invoke.reject("recording must be started by the visible app")
      return
    }
    if (!serviceDeclared()) {
      invoke.reject("recording service is unavailable")
      return
    }
    if (!CaptureStorage.isReady(availableStorageBytes())) {
      invoke.reject("not enough free storage for a two-hour recording")
      return
    }
    if (RecordingService.isActive() || currentStatus().optString("state") != SessionState.IDLE.name) {
      invoke.reject("a previous recording must be recovered before starting")
      return
    }
    val sessionId = UUID.randomUUID().toString()
    val intent = Intent(activity, RecordingService::class.java)
      .setAction(RecordingService.ACTION_START)
      .putExtra(RecordingService.EXTRA_SESSION_ID, sessionId)
    ContextCompat.startForegroundService(activity, intent)
    invoke.resolve(JSObject().put("sessionId", sessionId).put("state", "STARTING"))
  }

  @Command
  fun pause(invoke: Invoke) = sendControl(invoke, RecordingService.ACTION_PAUSE)

  @Command
  fun resume(invoke: Invoke) = sendControl(invoke, RecordingService.ACTION_RESUME)

  @Command
  fun stop(invoke: Invoke) = sendControl(invoke, RecordingService.ACTION_STOP)

  @Command
  fun status(invoke: Invoke) {
    val result = JSObject.fromJSONObject(currentStatus())
    result.put("imports", ImportInbox.pending(activity))
    result.put("captureScope", "microphone")
    invoke.resolve(result)
  }

  @Command
  fun openAudioPicker(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
      .addCategory(Intent.CATEGORY_OPENABLE)
      .setType("audio/*")
      .putExtra(
        Intent.EXTRA_MIME_TYPES,
        arrayOf("audio/mp4", "audio/m4a", "audio/x-m4a", "audio/mpeg", "audio/wav", "audio/x-wav"),
      )
      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
    startActivityForResult(invoke, intent, "audioPickerResult")
  }

  @Command
  fun acknowledgeSharedImports(invoke: Invoke) {
    try {
      val values = invoke.getArgs().optJSONArray("importIds")
        ?: throw IllegalArgumentException("missing import acknowledgement IDs")
      val importIds = buildList {
        repeat(values.length()) { index -> add(values.getString(index)) }
      }
      invoke.resolve(JSObject().put("removed", ImportInbox.acknowledge(activity, importIds)))
    } catch (_: Exception) {
      invoke.reject("shared import acknowledgement was rejected")
    }
  }

  @Command
  fun exportAudio(invoke: Invoke) {
    try {
      val args = invoke.getArgs()
      val sourcePath = args.getString("sourcePath")
      val fileName = ExportAudio.validateFileName(args.getString("fileName"))
      val expectedSizeBytes = args.getLong("expectedSizeBytes")
      val expectedSha256 = args.getString("expectedSha256")
      ExportAudio.validateSource(activity, sourcePath, expectedSizeBytes, expectedSha256)
      val intent = Intent(Intent.ACTION_CREATE_DOCUMENT)
        .addCategory(Intent.CATEGORY_OPENABLE)
        .setType(ExportAudio.mimeFor(fileName))
        .putExtra(Intent.EXTRA_TITLE, fileName)
      startActivityForResult(invoke, intent, "exportAudioResult")
    } catch (_: Exception) {
      invoke.reject("audio export was rejected")
    }
  }

  @ActivityCallback
  fun exportAudioResult(invoke: Invoke, result: ActivityResult) {
    if (result.resultCode == Activity.RESULT_CANCELED) {
      invoke.resolve(JSObject().put("exported", false))
      return
    }
    try {
      require(result.resultCode == Activity.RESULT_OK)
      val destination = requireNotNull(result.data?.data)
      val args = invoke.getArgs()
      ExportAudio.writeVerified(
        activity,
        destination,
        args.getString("sourcePath"),
        args.getLong("expectedSizeBytes"),
        args.getString("expectedSha256"),
      )
      invoke.resolve(JSObject().put("exported", true))
    } catch (_: Exception) {
      invoke.reject("audio export failed verification")
    }
  }

  @ActivityCallback
  fun audioPickerResult(invoke: Invoke, result: ActivityResult) {
    if (result.resultCode != Activity.RESULT_OK) {
      invoke.reject("audio picker cancelled")
      return
    }
    val uri = result.data?.data
    if (uri == null || uri.scheme != "content") {
      invoke.reject("audio picker returned no content URI")
      return
    }
    try {
      result.data?.flags?.and(Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)?.let { flags ->
        if (flags != 0) activity.contentResolver.takePersistableUriPermission(
          uri,
          flags and (Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION),
        )
      }
    } catch (_: SecurityException) {
      // The bytes are copied immediately; a persistable grant is optional.
    }
    try {
      invoke.resolve(JSObject.fromJSONObject(ImportInbox.copyPickedUri(activity, uri)))
    } catch (_: Exception) {
      invoke.reject("audio import was rejected")
    }
  }

  override fun onNewIntent(intent: Intent) {
    ImportInbox.consumeIntentAsync(activity, intent)
  }

  private fun sendControl(invoke: Invoke, action: String) {
    if (!RecordingService.isActive()) {
      currentStatus()
      invoke.reject("recording service is not active")
      return
    }
    val state = currentStatus().optString("state", SessionState.IDLE.name)
    val allowed = when (action) {
      RecordingService.ACTION_PAUSE -> state == SessionState.RECORDING.name
      RecordingService.ACTION_RESUME -> state == SessionState.PAUSED.name
      RecordingService.ACTION_STOP -> state == SessionState.RECORDING.name || state == SessionState.PAUSED.name
      else -> false
    }
    if (!allowed) {
      invoke.reject("recording control is invalid for the current state")
      return
    }
    activity.startService(Intent(activity, RecordingService::class.java).setAction(action))
    invoke.resolve(JSObject().put("accepted", true))
  }

  private fun permissionPayload(): JSObject {
    val microphone = ActivityCompat.checkSelfPermission(
      activity,
      Manifest.permission.RECORD_AUDIO,
    ) == PackageManager.PERMISSION_GRANTED
    val notifications = Build.VERSION.SDK_INT < 33 || ActivityCompat.checkSelfPermission(
      activity,
      Manifest.permission.POST_NOTIFICATIONS,
    ) == PackageManager.PERMISSION_GRANTED
    return JSObject()
      .put("recordAudio", microphone)
      .put("notifications", notifications)
  }

  private fun currentStatus(): org.json.JSONObject {
    val journal = SessionJournal(activity)
    val active = RecordingService.isActive()
    val result = if (active) journal.readSnapshot() else journal.recoverInterrupted()
    if (active) result.put("microphoneLevel", RecordingService.latestLevel().toDouble())
    return result
  }

  private fun availableStorageBytes(): Long = try {
    StatFs(activity.filesDir.absolutePath).availableBytes
  } catch (_: Exception) {
    -1L
  }

  private fun serviceDeclared(): Boolean {
    return try {
      if (Build.VERSION.SDK_INT >= 33) {
        activity.packageManager.getServiceInfo(
          ComponentName(activity, RecordingService::class.java),
          PackageManager.ComponentInfoFlags.of(0),
        )
      } else {
        @Suppress("DEPRECATION")
        activity.packageManager.getServiceInfo(
          ComponentName(activity, RecordingService::class.java),
          0,
        )
      }
      true
    } catch (_: Exception) {
      false
    }
  }
}
