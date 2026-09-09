package ai.ax.watch_transcriber.capture

import java.io.File
import java.io.RandomAccessFile
import java.security.MessageDigest
import java.util.ArrayDeque
import kotlin.math.sqrt

enum class SessionState {
  IDLE,
  RECORDING,
  PAUSED,
  STOPPED,
  INTERRUPTED,
}

object SessionTransitions {
  private val allowed = mapOf(
    SessionState.IDLE to setOf(SessionState.RECORDING),
    SessionState.RECORDING to setOf(SessionState.PAUSED, SessionState.STOPPED, SessionState.INTERRUPTED),
    SessionState.PAUSED to setOf(SessionState.RECORDING, SessionState.STOPPED, SessionState.INTERRUPTED),
    SessionState.STOPPED to emptySet(),
    SessionState.INTERRUPTED to emptySet(),
  )

  fun transition(current: SessionState, next: SessionState): SessionState {
    require(next in allowed.getValue(current)) { "invalid session transition" }
    return next
  }
}

object PcmSegments {
  fun maximumBytes(
    sampleRate: Int,
    channels: Int,
    bitsPerSample: Int,
    seconds: Int = 300,
  ): Long {
    require(sampleRate > 0 && channels > 0 && bitsPerSample == 16 && seconds in 1..300)
    return sampleRate.toLong() * channels * (bitsPerSample / 8) * seconds
  }

  fun writableBytes(currentBytes: Long, availableBytes: Int, maximumBytes: Long): Int {
    require(currentBytes in 0..maximumBytes && availableBytes >= 0 && maximumBytes > 0)
    return minOf(availableBytes.toLong(), maximumBytes - currentBytes).toInt()
  }
}

object PcmLevels {
  fun rms16le(bytes: ByteArray, count: Int): Float {
    require(count in 0..bytes.size)
    val sampleCount = count / 2
    if (sampleCount == 0) return 0f
    var sumSquares = 0.0
    repeat(sampleCount) { index ->
      val offset = index * 2
      val sample = ((bytes[offset].toInt() and 0xff) or (bytes[offset + 1].toInt() shl 8)).toShort()
      val normalized = sample.toDouble() / Short.MAX_VALUE
      sumSquares += normalized * normalized
    }
    return sqrt(sumSquares / sampleCount).coerceIn(0.0, 1.0).toFloat()
  }
}

object CaptureStorage {
  const val MINIMUM_FREE_BYTES = 1024L * 1024L * 1024L

  fun isReady(availableBytes: Long): Boolean = availableBytes >= MINIMUM_FREE_BYTES
}

object SafeDisplayName {
  fun sanitize(value: String?, fallback: String = "recording"): String {
    val normalized = value.orEmpty()
      .replace(Regex("[\\p{Cc}\\p{Cf}/\\\\]+"), "-")
      .replace(Regex("[^\\p{L}\\p{N} ._()-]+"), "-")
      .replace(Regex("[- ]{2,}"), "-")
      .trim(' ', '-', '.')
      .take(96)
    return normalized.ifEmpty { fallback }
  }
}

class IntentTokenLedger(private val capacity: Int = 1024) {
  private val order = ArrayDeque<String>()
  private val tokens = HashSet<String>()

  init {
    require(capacity in 1..10_000) { "invalid ledger capacity" }
  }

  @Synchronized
  fun accept(token: String): Boolean {
    require(token.matches(Regex("[0-9a-f]{64}"))) { "invalid intent token" }
    if (!tokens.add(token)) return false
    order.addLast(token)
    while (order.size > capacity) tokens.remove(order.removeFirst())
    return true
  }

  @Synchronized
  fun seed(values: Iterable<String>) {
    values.forEach { value -> if (value.matches(Regex("[0-9a-f]{64}"))) accept(value) }
  }
}

object IntentToken {
  fun from(action: String?, uris: Collection<String>): String {
    val canonical = buildString {
      append(action.orEmpty())
      uris.sorted().forEach { append('\n').append(it) }
    }
    return MessageDigest.getInstance("SHA-256")
      .digest(canonical.toByteArray(Charsets.UTF_8))
      .joinToString("") { "%02x".format(it) }
  }
}

enum class SupportedAudio(val extension: String, val mimeTypes: Set<String>) {
  WAV("wav", setOf("audio/wav", "audio/x-wav", "audio/wave", "audio/vnd.wave")),
  MP3("mp3", setOf("audio/mpeg", "audio/mp3")),
  M4A("m4a", setOf("audio/mp4", "audio/m4a", "audio/x-m4a")),
}

object AudioContent {
  fun identify(prefix: ByteArray, declaredMime: String?): SupportedAudio {
    val media = when {
      prefix.size >= 12 && prefix.copyOfRange(0, 4).contentEquals("RIFF".toByteArray()) &&
        prefix.copyOfRange(8, 12).contentEquals("WAVE".toByteArray()) -> SupportedAudio.WAV
      prefix.size >= 3 && prefix.copyOfRange(0, 3).contentEquals("ID3".toByteArray()) -> SupportedAudio.MP3
      prefix.size >= 2 && prefix[0] == 0xff.toByte() && (prefix[1].toInt() and 0xe0) == 0xe0 -> SupportedAudio.MP3
      prefix.size >= 12 && prefix.copyOfRange(4, 8).contentEquals("ftyp".toByteArray()) -> SupportedAudio.M4A
      else -> throw IllegalArgumentException("unsupported audio content")
    }
    val normalizedMime = declaredMime?.substringBefore(';')?.trim()?.lowercase()
    require(normalizedMime == null || normalizedMime in media.mimeTypes) {
      "audio MIME does not match content"
    }
    return media
  }
}

object WavFiles {
  const val HEADER_BYTES = 44

  fun initialize(file: File, sampleRate: Int, channels: Int, bitsPerSample: Int) {
    require(sampleRate in 8_000..192_000)
    require(channels in 1..2)
    require(bitsPerSample == 16)
    RandomAccessFile(file, "rw").use { stream ->
      stream.setLength(HEADER_BYTES.toLong())
      writeHeader(stream, 0, sampleRate, channels, bitsPerSample)
      stream.fd.sync()
    }
  }

  fun repair(file: File, sampleRate: Int, channels: Int, bitsPerSample: Int): Long {
    require(file.isFile)
    RandomAccessFile(file, "rw").use { stream ->
      val dataBytes = (stream.length() - HEADER_BYTES).coerceAtLeast(0)
      require(dataBytes <= 0xfffffff0L) { "WAV segment is too large" }
      if (stream.length() < HEADER_BYTES) stream.setLength(HEADER_BYTES.toLong())
      writeHeader(stream, dataBytes, sampleRate, channels, bitsPerSample)
      stream.fd.sync()
      return dataBytes
    }
  }

  private fun writeHeader(
    stream: RandomAccessFile,
    dataBytes: Long,
    sampleRate: Int,
    channels: Int,
    bitsPerSample: Int,
  ) {
    val byteRate = sampleRate * channels * bitsPerSample / 8
    val blockAlign = channels * bitsPerSample / 8
    stream.seek(0)
    stream.writeBytes("RIFF")
    writeLe32(stream, 36 + dataBytes)
    stream.writeBytes("WAVEfmt ")
    writeLe32(stream, 16)
    writeLe16(stream, 1)
    writeLe16(stream, channels)
    writeLe32(stream, sampleRate.toLong())
    writeLe32(stream, byteRate.toLong())
    writeLe16(stream, blockAlign)
    writeLe16(stream, bitsPerSample)
    stream.writeBytes("data")
    writeLe32(stream, dataBytes)
  }

  private fun writeLe16(stream: RandomAccessFile, value: Int) {
    stream.write(value and 0xff)
    stream.write((value ushr 8) and 0xff)
  }

  private fun writeLe32(stream: RandomAccessFile, value: Long) {
    repeat(4) { shift -> stream.write(((value ushr (shift * 8)) and 0xff).toInt()) }
  }
}
