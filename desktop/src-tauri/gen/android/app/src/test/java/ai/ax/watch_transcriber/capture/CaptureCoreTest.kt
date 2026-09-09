package ai.ax.watch_transcriber.capture

import java.io.RandomAccessFile
import java.security.MessageDigest
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureCoreTest {
  @Test
  fun repairsWavHeaderFromDurablePcmBytes() {
    val path = Files.createTempFile("echowall-wav", ".wav").toFile()
    try {
      WavFiles.initialize(path, 16_000, 1, 16)
      RandomAccessFile(path, "rw").use { file ->
        file.seek(file.length())
        file.write(ByteArray(32_000))
      }
      assertEquals(32_000, WavFiles.repair(path, 16_000, 1, 16))
      RandomAccessFile(path, "r").use { file ->
        file.seek(4)
        assertEquals(32_036, readLe32(file))
        file.seek(40)
        assertEquals(32_000, readLe32(file))
      }
    } finally {
      path.delete()
    }
  }

  @Test
  fun sanitizesUntrustedDisplayNamesWithoutCreatingPaths() {
    val name = SafeDisplayName.sanitize("../../secret\\\u0000 meeting?.m4a")
    assertFalse(name.contains('/'))
    assertFalse(name.contains('\\'))
    assertFalse(name.contains(".."))
    assertTrue(name.endsWith(".m4a"))
  }

  @Test
  fun sessionTransitionsRejectImpossibleEdges() {
    assertEquals(
      SessionState.RECORDING,
      SessionTransitions.transition(SessionState.IDLE, SessionState.RECORDING),
    )
    assertEquals(
      SessionState.PAUSED,
      SessionTransitions.transition(SessionState.RECORDING, SessionState.PAUSED),
    )
    assertThrows(IllegalArgumentException::class.java) {
      SessionTransitions.transition(SessionState.STOPPED, SessionState.RECORDING)
    }
  }

  @Test
  fun segmentBoundaryNeverExceedsFiveMinutesOfPcm() {
    val maximum = PcmSegments.maximumBytes(16_000, 1, 16)
    assertEquals(9_600_000, maximum)
    assertEquals(2, PcmSegments.writableBytes(maximum - 2, 8_192, maximum))
    assertEquals(0, PcmSegments.writableBytes(maximum, 8_192, maximum))
  }

  @Test
  fun pcmMeterIsNormalizedAndIgnoresTrailingPartialSample() {
    assertEquals(0f, PcmLevels.rms16le(byteArrayOf(), 0))
    assertEquals(0f, PcmLevels.rms16le(byteArrayOf(0, 0, 7), 3))
    val maximum = byteArrayOf(0xff.toByte(), 0x7f, 0x00, 0x80.toByte())
    assertEquals(1f, PcmLevels.rms16le(maximum, maximum.size), 0.0001f)
  }

  @Test
  fun twoHourMobileCaptureRequiresOneGibibyteFree() {
    assertFalse(CaptureStorage.isReady(CaptureStorage.MINIMUM_FREE_BYTES - 1))
    assertTrue(CaptureStorage.isReady(CaptureStorage.MINIMUM_FREE_BYTES))
  }

  @Test
  fun intentTokensAreOrderStableAndAcceptedOnlyOnce() {
    val first = IntentToken.from("send", listOf("content://b", "content://a"))
    val second = IntentToken.from("send", listOf("content://a", "content://b"))
    assertEquals(first, second)
    val ledger = IntentTokenLedger(2)
    assertTrue(ledger.accept(first))
    assertFalse(ledger.accept(second))
  }

  @Test
  fun audioContentRequiresMatchingWavMp3OrM4aBytesAndMime() {
    val wav = "RIFF1234WAVE".toByteArray()
    assertEquals(SupportedAudio.WAV, AudioContent.identify(wav, "audio/wav"))
    assertEquals(SupportedAudio.MP3, AudioContent.identify("ID3fixture".toByteArray(), "audio/mpeg"))
    assertEquals(
      SupportedAudio.M4A,
      AudioContent.identify(byteArrayOf(0, 0, 0, 16) + "ftypM4A ".toByteArray(), "audio/mp4"),
    )
    assertThrows(IllegalArgumentException::class.java) {
      AudioContent.identify(wav, "audio/mpeg")
    }
    assertThrows(IllegalArgumentException::class.java) {
      AudioContent.identify("not audio".toByteArray(), "audio/wav")
    }
  }

  @Test
  fun exportDigestAndFilenameValidationAreDeterministic() {
    val path = Files.createTempFile("echowall-export", ".wav").toFile()
    try {
      val bytes = "fabricated export audio".toByteArray()
      path.writeBytes(bytes)
      val expected = MessageDigest.getInstance("SHA-256")
        .digest(bytes)
        .joinToString("") { "%02x".format(it) }
      assertEquals(AudioDigest(bytes.size.toLong(), expected), ExportAudio.digest(path))
      assertEquals("recording.wav", ExportAudio.validateFileName("recording.wav"))
      assertThrows(IllegalArgumentException::class.java) {
        ExportAudio.validateFileName("../recording.wav")
      }
    } finally {
      path.delete()
    }
  }

  private fun readLe32(file: RandomAccessFile): Long {
    var value = 0L
    repeat(4) { shift -> value = value or (file.read().toLong() shl (shift * 8)) }
    return value
  }
}
