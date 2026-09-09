package ai.ax.watch_transcriber.capture

import android.content.Context
import java.io.File
import java.io.FileOutputStream
import org.json.JSONObject

class SessionJournal(private val context: Context) {
  private val root = File(context.filesDir, "capture")
  private val sessions = File(root, "sessions")
  private val snapshot = File(root, "active-session.json")
  private val events = File(root, "session-events.ndjson")

  init {
    sessions.mkdirs()
  }

  @Synchronized
  fun recoverInterrupted(): JSONObject {
    val current = readSnapshot()
    current.optString("sessionId").takeIf { it.isNotEmpty() }?.let { sessionId ->
      File(sessions, sessionId).listFiles { file -> file.extension == "wav" }
        ?.forEach { WavFiles.repair(it, RecordingService.SAMPLE_RATE, 1, 16) }
    }
    val state = current.optString("state", SessionState.IDLE.name)
    if (state == SessionState.RECORDING.name || state == SessionState.PAUSED.name) {
      current.put("state", SessionState.INTERRUPTED.name)
      current.put("updatedAtMs", System.currentTimeMillis())
      writeSnapshot(current)
      appendEvent("interrupted", current.optString("sessionId"), null)
    }
    return current
  }

  @Synchronized
  fun begin(sessionId: String): File {
    require(sessionId.matches(Regex("[0-9a-f-]{36}")))
    val directory = File(sessions, sessionId)
    check(directory.mkdirs() || directory.isDirectory)
    val now = System.currentTimeMillis()
    writeSnapshot(
      JSONObject()
        .put("sessionId", sessionId)
        .put("relativeDirectory", "capture/sessions/$sessionId")
        .put("state", SessionState.RECORDING.name)
        .put("startedAtMs", now)
        .put("updatedAtMs", now)
        .put("segmentIndex", 0)
        .put("totalPcmBytes", 0),
    )
    appendEvent("started", sessionId, null)
    return directory
  }

  @Synchronized
  fun transition(next: SessionState, detail: JSONObject? = null): JSONObject {
    val current = readSnapshot()
    val currentState = SessionState.valueOf(
      current.optString("state", SessionState.IDLE.name),
    )
    SessionTransitions.transition(currentState, next)
    current.put("state", next.name).put("updatedAtMs", System.currentTimeMillis())
    writeSnapshot(current)
    appendEvent(next.name.lowercase(), current.optString("sessionId"), detail)
    return current
  }

  @Synchronized
  fun checkpoint(segmentIndex: Int, totalPcmBytes: Long) {
    val current = readSnapshot()
    current
      .put("segmentIndex", segmentIndex)
      .put("totalPcmBytes", totalPcmBytes)
      .put("updatedAtMs", System.currentTimeMillis())
    writeSnapshot(current)
  }

  @Synchronized
  fun segmentCompleted(segmentIndex: Int, pcmBytes: Long) {
    appendEvent(
      "segment_completed",
      readSnapshot().optString("sessionId"),
      JSONObject()
        .put("segmentIndex", segmentIndex)
        .put("relativePath", "segment-${segmentIndex.toString().padStart(4, '0')}.wav")
        .put("pcmBytes", pcmBytes),
    )
  }

  @Synchronized
  fun readSnapshot(): JSONObject {
    return try {
      if (!snapshot.isFile) idleStatus() else JSONObject(snapshot.readText(Charsets.UTF_8))
    } catch (_: Exception) {
      idleStatus().put("recoveryError", true)
    }
  }

  private fun writeSnapshot(value: JSONObject) {
    root.mkdirs()
    val temporary = File(root, "active-session.json.tmp")
    FileOutputStream(temporary).use { output ->
      output.write(value.toString().toByteArray(Charsets.UTF_8))
      output.fd.sync()
    }
    check(temporary.renameTo(snapshot)) { "unable to checkpoint recording session" }
  }

  private fun appendEvent(kind: String, sessionId: String, detail: JSONObject?) {
    root.mkdirs()
    val event = JSONObject()
      .put("kind", kind)
      .put("sessionId", sessionId)
      .put("atMs", System.currentTimeMillis())
    if (detail != null) event.put("detail", detail)
    FileOutputStream(events, true).use { output ->
      output.write((event.toString() + "\n").toByteArray(Charsets.UTF_8))
      output.fd.sync()
    }
  }

  companion object {
    fun status(context: Context): JSONObject = SessionJournal(context).readSnapshot()

    private fun idleStatus(): JSONObject = JSONObject()
      .put("sessionId", JSONObject.NULL)
      .put("state", SessionState.IDLE.name)
      .put("segmentIndex", 0)
      .put("totalPcmBytes", 0)
  }
}
