package ai.ax.watch_transcriber.capture

import android.content.Context
import android.net.Uri
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.security.MessageDigest

data class AudioDigest(val sizeBytes: Long, val sha256: String)

object ExportAudio {
  private const val MAX_EXPORT_BYTES = 512L * 1024L * 1024L
  private val safeName = Regex("[^/\\\\\u0000-\u001f]{1,255}")

  fun validateSource(
    context: Context,
    sourcePath: String,
    expectedSizeBytes: Long,
    expectedSha256: String,
  ): File {
    require(expectedSizeBytes in 1..MAX_EXPORT_BYTES)
    require(expectedSha256.matches(Regex("[0-9a-f]{64}")))
    require(!sourcePath.contains('\u0000') && !sourcePath.contains('\\'))
    val supplied = File(sourcePath).absoluteFile
    val source = supplied.canonicalFile
    val roots = listOfNotNull(
      File(context.filesDir.absoluteFile, "inbox"),
      context.filesDir.absoluteFile.parentFile?.let { File(it, "inbox") },
    )
    val matched = roots.firstNotNullOfOrNull { rawRoot ->
      val canonicalRoot = rawRoot.canonicalFile
      val relative = when {
        supplied.path.startsWith(rawRoot.path + File.separator) ->
          supplied.path.removePrefix(rawRoot.path + File.separator)
        supplied.path.startsWith(canonicalRoot.path + File.separator) ->
          supplied.path.removePrefix(canonicalRoot.path + File.separator)
        else -> null
      } ?: return@firstNotNullOfOrNull null
      canonicalRoot to relative
    } ?: throw IllegalArgumentException("source is outside the Rust inbox")
    var checked = matched.first
    val components = matched.second.split('/').filter(String::isNotEmpty)
    require(components.isNotEmpty() && components.none { it == "." || it == ".." })
    components.forEach { component ->
      checked = File(checked, component).absoluteFile
      require(checked.canonicalFile.path == checked.path) {
        "source contains a symbolic link"
      }
    }
    require(checked.path == source.path && source.isFile)
    require(digest(source) == AudioDigest(expectedSizeBytes, expectedSha256))
    return source
  }

  fun validateFileName(fileName: String): String {
    require(safeName.matches(fileName))
    require(fileName.substringAfterLast('.', "").lowercase() in setOf("m4a", "mp3", "wav"))
    return fileName
  }

  fun mimeFor(fileName: String): String = when (fileName.substringAfterLast('.').lowercase()) {
    "m4a" -> "audio/mp4"
    "mp3" -> "audio/mpeg"
    else -> "audio/wav"
  }

  fun writeVerified(
    context: Context,
    destination: Uri,
    sourcePath: String,
    expectedSizeBytes: Long,
    expectedSha256: String,
  ) {
    require(destination.scheme == "content")
    val source = validateSource(context, sourcePath, expectedSizeBytes, expectedSha256)
    try {
      FileInputStream(source).use { input ->
        val output = requireNotNull(context.contentResolver.openOutputStream(destination, "w"))
        output.use {
          input.copyTo(it, 64 * 1024)
          it.flush()
          if (it is FileOutputStream) it.fd.sync()
        }
      }
      val written = requireNotNull(context.contentResolver.openInputStream(destination)).use { input ->
        digest(input)
      }
      require(written == AudioDigest(expectedSizeBytes, expectedSha256))
    } catch (error: Exception) {
      try {
        context.contentResolver.delete(destination, null, null)
      } catch (_: Exception) {
      }
      throw error
    }
  }

  fun digest(file: File): AudioDigest = FileInputStream(file).use(::digest)

  private fun digest(input: java.io.InputStream): AudioDigest {
    val hash = MessageDigest.getInstance("SHA-256")
    val buffer = ByteArray(64 * 1024)
    var size = 0L
    while (true) {
      val count = input.read(buffer)
      if (count < 0) break
      if (count == 0) continue
      size += count
      require(size <= MAX_EXPORT_BYTES)
      hash.update(buffer, 0, count)
    }
    return AudioDigest(size, hash.digest().joinToString("") { "%02x".format(it) })
  }
}
