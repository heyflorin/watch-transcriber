package ai.ax.watch_transcriber.capture

import android.content.ContentValues
import android.os.Environment
import android.provider.MediaStore
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.SdkSuppress
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.MessageDigest
import java.util.UUID
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
@SdkSuppress(minSdkVersion = 29)
class ImportInboxInstrumentedTest {
  @Test
  fun copiesReplaysAndAcknowledgesRealContentUri() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    val inbox = File(context.filesDir, "inbox")
    inbox.deleteRecursively()
    val source = File(context.cacheDir, "echowall-import-${UUID.randomUUID()}.wav")
    WavFiles.initialize(source, RecordingService.SAMPLE_RATE, 1, 16)
    source.appendBytes(ByteArray(32_000) { index -> (index % 113).toByte() })
    WavFiles.repair(source, RecordingService.SAMPLE_RATE, 1, 16)
    val bytes = source.readBytes()
    val sha256 = MessageDigest.getInstance("SHA-256")
      .digest(bytes)
      .joinToString("") { "%02x".format(it) }
    val displayName = "EchoWall Synthetic Import ${UUID.randomUUID()}.wav"
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

      val imported = ImportInbox.copyPickedUri(context, uri)
      val importId = imported.getString("importId")
      assertEquals(displayName, imported.getString("displayName"))
      assertEquals("audio/wav", imported.getString("mime"))
      assertEquals(bytes.size.toLong(), imported.getLong("sizeBytes"))
      assertEquals(sha256, imported.getString("sha256"))
      assertEquals("ready", imported.getString("state"))
      val copied = File(context.filesDir, imported.getString("relativePath"))
      assertTrue(copied.isFile)
      assertArrayEquals(bytes, copied.readBytes())

      val replay = ImportInbox.copyPickedUri(context, uri)
      assertEquals(importId, replay.getString("importId"))
      assertEquals(1, ImportInbox.pending(context).length())
      assertEquals(1, ImportInbox.acknowledge(context, listOf(importId)))
      assertFalse(copied.exists())
      assertEquals(0, ImportInbox.pending(context).length())
    } finally {
      resolver.delete(uri, null, null)
      source.delete()
      inbox.deleteRecursively()
    }
  }
}
