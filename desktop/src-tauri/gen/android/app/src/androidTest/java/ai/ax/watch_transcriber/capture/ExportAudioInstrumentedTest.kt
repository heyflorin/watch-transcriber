package ai.ax.watch_transcriber.capture

import android.content.ContentValues
import android.os.Environment
import android.provider.MediaStore
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.SdkSuppress
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.util.UUID
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
@SdkSuppress(minSdkVersion = 29)
class ExportAudioInstrumentedTest {
  @Test
  fun writesAndReopensVerifiedContentUriFromRustInboxSource() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    val packageDirectory = File(
      requireNotNull(context.filesDir.parentFile),
      "inbox/${UUID.randomUUID()}",
    )
    check(packageDirectory.mkdirs())
    val source = File(packageDirectory, "source.wav")
    val bytes = ByteArray(64 * 1024) { index -> (index % 251).toByte() }
    source.writeBytes(bytes)
    val digest = ExportAudio.digest(source)
    val displayName = "echowall-export-${UUID.randomUUID()}.wav"
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
    val destination = requireNotNull(
      resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values),
    )
    try {
      ExportAudio.writeVerified(
        context,
        destination,
        source.absolutePath,
        digest.sizeBytes,
        digest.sha256,
      )
      val exported = requireNotNull(resolver.openInputStream(destination)).use { it.readBytes() }
      assertArrayEquals(bytes, exported)
      assertEquals(digest, ExportAudio.digest(source))
    } finally {
      resolver.delete(destination, null, null)
      packageDirectory.deleteRecursively()
    }
  }
}
