package ai.ax.watch_transcriber.capture

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import android.os.IBinder
import android.os.StatFs
import androidx.core.app.ActivityCompat
import androidx.core.app.NotificationCompat
import java.io.File
import java.io.RandomAccessFile
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlin.concurrent.thread

class RecordingService : Service() {
  private lateinit var journal: SessionJournal
  private val running = AtomicBoolean(false)
  private val paused = AtomicBoolean(false)
  private val stopRequested = AtomicBoolean(false)

  override fun onCreate() {
    super.onCreate()
    journal = SessionJournal(this)
    journal.recoverInterrupted()
    ensureNotificationChannel()
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    when (intent?.action) {
      ACTION_START -> startUserSession(intent.getStringExtra(EXTRA_SESSION_ID))
      ACTION_PAUSE -> pauseSession()
      ACTION_RESUME -> resumeSession()
      ACTION_STOP -> stopSession()
    }
    return START_NOT_STICKY
  }

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onDestroy() {
    running.set(false)
    activeService = false
    super.onDestroy()
  }

  private fun startUserSession(requestedId: String?) {
    if (running.get()) return
    if (ActivityCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
      stopSelf()
      return
    }
    if (!hasCaptureCapacity()) {
      stopSelf()
      return
    }
    val sessionId = requestedId
      ?.takeIf { it.matches(Regex("[0-9a-f-]{36}")) }
      ?: UUID.randomUUID().toString()
    val directory = journal.begin(sessionId)
    running.set(true)
    activeService = true
    latestLevelBits.set(0f.toBits())
    paused.set(false)
    stopRequested.set(false)
    startMicrophoneForeground(notification(paused = false))
    thread(name = "EchoWallMicCapture", isDaemon = false) {
      recordLoop(directory)
    }
  }

  private fun pauseSession() {
    if (!running.get()) {
      stopSelf()
      return
    }
    if (!paused.compareAndSet(false, true)) return
    latestLevelBits.set(0f.toBits())
    journal.transition(SessionState.PAUSED)
    notifyState(paused = true)
  }

  private fun resumeSession() {
    if (!running.get()) {
      stopSelf()
      return
    }
    if (!paused.compareAndSet(true, false)) return
    journal.transition(SessionState.RECORDING)
    notifyState(paused = false)
  }

  private fun stopSession() {
    if (!running.get()) {
      stopSelf()
      return
    }
    stopRequested.set(true)
    running.set(false)
  }

  private fun recordLoop(directory: File) {
    if (ActivityCompat.checkSelfPermission(
        this,
        Manifest.permission.RECORD_AUDIO,
      ) != PackageManager.PERMISSION_GRANTED
    ) {
      interruptAndStop("microphone_permission_revoked")
      return
    }
    val minimum = AudioRecord.getMinBufferSize(
      SAMPLE_RATE,
      AudioFormat.CHANNEL_IN_MONO,
      AudioFormat.ENCODING_PCM_16BIT,
    )
    if (minimum <= 0) {
      interruptAndStop("invalid_audio_buffer")
      return
    }
    val buffer = ByteArray(maxOf(minimum * 2, 8_192))
    val recorder = try {
      AudioRecord(
        MediaRecorder.AudioSource.MIC,
        SAMPLE_RATE,
        AudioFormat.CHANNEL_IN_MONO,
        AudioFormat.ENCODING_PCM_16BIT,
        buffer.size,
      )
    } catch (_: SecurityException) {
      interruptAndStop("microphone_permission_revoked")
      return
    }
    if (recorder.state != AudioRecord.STATE_INITIALIZED) {
      recorder.release()
      interruptAndStop("microphone_initialization_failed")
      return
    }

    var segmentIndex = 0
    var totalBytes = 0L
    var segment: RandomAccessFile? = null
    var segmentFile: File? = null
    var recordingActive = false
    var lastRepairAt = 0L
    try {
      while (running.get()) {
        if (paused.get()) {
          latestLevelBits.set(0f.toBits())
          if (recordingActive) {
            recorder.stop()
            recordingActive = false
            segmentFile?.let { WavFiles.repair(it, SAMPLE_RATE, 1, 16) }
          }
          Thread.sleep(100)
          continue
        }
        if (!recordingActive) {
          recorder.startRecording()
          recordingActive = true
        }
        val read = recorder.read(buffer, 0, buffer.size, AudioRecord.READ_BLOCKING)
        if (read < 0) throw IllegalStateException("microphone read failed")
        if (read == 0) continue
        if (!running.get()) break
        if (paused.get()) {
          latestLevelBits.set(0f.toBits())
          continue
        }
        latestLevelBits.set(PcmLevels.rms16le(buffer, read).toBits())
        var offset = 0
        while (offset < read) {
          if (segment == null) {
            segmentFile = File(
              directory,
              "segment-${segmentIndex.toString().padStart(4, '0')}.wav",
            )
            WavFiles.initialize(segmentFile, SAMPLE_RATE, 1, 16)
            segment = RandomAccessFile(segmentFile, "rw").apply { seek(length()) }
            lastRepairAt = System.currentTimeMillis()
          }
          val activeSegment = checkNotNull(segment)
          val segmentBytes = activeSegment.length() - WavFiles.HEADER_BYTES
          val writable = PcmSegments.writableBytes(
            segmentBytes,
            read - offset,
            SEGMENT_PCM_BYTES,
          )
          activeSegment.write(buffer, offset, writable)
          offset += writable
          totalBytes += writable
          val updatedSegmentBytes = segmentBytes + writable
          val now = System.currentTimeMillis()
          if (now - lastRepairAt >= HEADER_REPAIR_INTERVAL_MS) {
            activeSegment.fd.sync()
            segmentFile?.let { WavFiles.repair(it, SAMPLE_RATE, 1, 16) }
            activeSegment.seek(activeSegment.length())
            journal.checkpoint(segmentIndex, totalBytes)
            lastRepairAt = now
          }
          if (updatedSegmentBytes == SEGMENT_PCM_BYTES) {
            activeSegment.fd.sync()
            activeSegment.close()
            segmentFile?.let { WavFiles.repair(it, SAMPLE_RATE, 1, 16) }
            journal.segmentCompleted(segmentIndex, updatedSegmentBytes)
            segment = null
            segmentFile = null
            segmentIndex += 1
            journal.checkpoint(segmentIndex, totalBytes)
          }
        }
      }
      if (recordingActive) recorder.stop()
      segment?.fd?.sync()
      segment?.close()
      segmentFile?.let { finalFile ->
        WavFiles.repair(finalFile, SAMPLE_RATE, 1, 16)
        val finalBytes = (finalFile.length() - WavFiles.HEADER_BYTES).coerceAtLeast(0)
        journal.segmentCompleted(segmentIndex, finalBytes)
      }
      journal.checkpoint(segmentIndex, totalBytes)
      val state = SessionState.valueOf(journal.readSnapshot().getString("state"))
      if (state == SessionState.RECORDING || state == SessionState.PAUSED) {
        journal.transition(
          if (stopRequested.get()) SessionState.STOPPED else SessionState.INTERRUPTED,
        )
      }
    } catch (_: InterruptedException) {
      Thread.currentThread().interrupt()
      markInterrupted("capture_thread_interrupted")
    } catch (_: Exception) {
      markInterrupted(if (hasCaptureCapacity()) "capture_failed" else "recording_storage_full")
    } finally {
      try {
        if (recordingActive && recorder.recordingState == AudioRecord.RECORDSTATE_RECORDING) recorder.stop()
      } catch (_: Exception) {
      }
      try {
        segment?.close()
      } catch (_: Exception) {
      }
      recorder.release()
      running.set(false)
      activeService = false
      latestLevelBits.set(0f.toBits())
      stopForeground(STOP_FOREGROUND_REMOVE)
      stopSelf()
    }
  }

  private fun interruptAndStop(code: String) {
    markInterrupted(code)
    running.set(false)
    stopForeground(STOP_FOREGROUND_REMOVE)
    stopSelf()
  }

  private fun markInterrupted(code: String) {
    val state = SessionState.valueOf(journal.readSnapshot().optString("state", SessionState.IDLE.name))
    if (state == SessionState.RECORDING || state == SessionState.PAUSED) {
      journal.transition(
        SessionState.INTERRUPTED,
        org.json.JSONObject().put("code", code),
      )
    }
  }

  private fun hasCaptureCapacity(): Boolean = try {
    CaptureStorage.isReady(StatFs(filesDir.absolutePath).availableBytes)
  } catch (_: Exception) {
    false
  }

  private fun startMicrophoneForeground(value: Notification) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      startForeground(NOTIFICATION_ID, value, ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
    } else {
      startForeground(NOTIFICATION_ID, value)
    }
  }

  private fun notifyState(paused: Boolean) {
    getSystemService(NotificationManager::class.java).notify(
      NOTIFICATION_ID,
      notification(paused),
    )
  }

  private fun notification(paused: Boolean): Notification {
    val toggleAction = if (paused) ACTION_RESUME else ACTION_PAUSE
    val toggleLabel = if (paused) "Resume" else "Pause"
    return NotificationCompat.Builder(this, CHANNEL_ID)
      .setSmallIcon(android.R.drawable.ic_btn_speak_now)
      .setContentTitle(getString(ai.ax.watch_transcriber.R.string.recording_notification_title))
      .setContentText(if (paused) "Recording paused" else "Microphone recording in progress")
      .setOngoing(true)
      .setOnlyAlertOnce(true)
      .setCategory(NotificationCompat.CATEGORY_SERVICE)
      .addAction(0, toggleLabel, serviceIntent(toggleAction, 1))
      .addAction(0, "Stop", serviceIntent(ACTION_STOP, 2))
      .build()
  }

  private fun serviceIntent(action: String, requestCode: Int): PendingIntent {
    val intent = Intent(this, RecordingService::class.java).setAction(action)
    return PendingIntent.getService(
      this,
      requestCode,
      intent,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
  }

  private fun ensureNotificationChannel() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val manager = getSystemService(NotificationManager::class.java)
    manager.createNotificationChannel(
      NotificationChannel(
        CHANNEL_ID,
        getString(ai.ax.watch_transcriber.R.string.recording_notification_channel),
        NotificationManager.IMPORTANCE_LOW,
      ),
    )
  }

  companion object {
    const val SAMPLE_RATE = 16_000
    const val ACTION_START = "ai.ax.watch_transcriber.capture.START"
    const val ACTION_PAUSE = "ai.ax.watch_transcriber.capture.PAUSE"
    const val ACTION_RESUME = "ai.ax.watch_transcriber.capture.RESUME"
    const val ACTION_STOP = "ai.ax.watch_transcriber.capture.STOP"
    const val EXTRA_SESSION_ID = "session_id"
    private const val CHANNEL_ID = "echowall_recording"
    private const val NOTIFICATION_ID = 41_001
    private const val HEADER_REPAIR_INTERVAL_MS = 5_000L
    private val SEGMENT_PCM_BYTES = PcmSegments.maximumBytes(SAMPLE_RATE, 1, 16)
    @Volatile private var activeService = false
    private val latestLevelBits = AtomicInteger(0f.toBits())

    fun isActive(): Boolean = activeService
    fun latestLevel(): Float = Float.fromBits(latestLevelBits.get()).coerceIn(0f, 1f)
  }
}
