package ai.ax.watch_transcriber.capture

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.media.MediaExtractor
import android.media.MediaFormat
import android.os.Build
import android.provider.OpenableColumns
import java.io.File
import java.io.FileOutputStream
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.Executors
import org.json.JSONArray
import org.json.JSONObject

object ImportInbox {
  const val MAX_IMPORT_BYTES = 512L * 1024L * 1024L
  const val MAX_INTENT_ITEMS = 16
  private val executor = Executors.newSingleThreadExecutor()
  private val lock = Any()
  private val importIdPattern = Regex(
    "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
  )
  private val supportedExtensions = listOf("m4a", "mp3", "wav")

  fun consumeIntentAsync(context: Context, intent: Intent?) {
    val safeIntent = intent ?: return
    if (safeIntent.action !in setOf(Intent.ACTION_SEND, Intent.ACTION_SEND_MULTIPLE)) return
    val safeContext = context.applicationContext
    executor.execute {
      try {
        consumeIntent(safeContext, safeIntent)
      } catch (_: Exception) {
        // The durable inbox remains unchanged; a redelivered intent may retry.
      }
    }
  }

  fun consumeIntent(context: Context, intent: Intent): JSONArray {
    val uris = intentUris(intent)
    require(uris.isNotEmpty() && uris.size <= MAX_INTENT_ITEMS) { "invalid shared audio count" }
    val token = IntentToken.from(intent.action, uris.map(Uri::toString))
    synchronized(lock) {
      val root = inboxRoot(context)
      val acceptedTokens = readTokens(File(root, "intent-tokens.ndjson"))
      if (token in acceptedTokens) return JSONArray()
      val results = JSONArray()
      uris.forEach { uri -> results.put(copyUri(context, uri, token)) }
      appendLine(File(root, "intent-tokens.ndjson"), token)
      return results
    }
  }

  fun copyPickedUri(context: Context, uri: Uri): JSONObject {
    require(uri.scheme == "content") { "only content URIs are accepted" }
    val token = IntentToken.from("picker", listOf(uri.toString()))
    synchronized(lock) {
      return copyUri(context.applicationContext, uri, token)
    }
  }

  fun pending(context: Context): JSONArray {
    synchronized(lock) {
      val root = inboxRoot(context)
      val file = File(root, "imports.ndjson")
      val result = JSONArray()
      if (!file.isFile) return result
      val acknowledged = readImportIds(File(root, "acknowledged-imports.ndjson"))
      acknowledged.forEach { cleanupAcknowledgedFile(root, it) }
      tailLines(file, 1_024).forEach { line ->
        try {
          val item = JSONObject(line)
          if (item.optString("importId") !in acknowledged) result.put(item)
        } catch (_: Exception) {
        }
      }
      return result
    }
  }

  fun acknowledge(context: Context, importIds: List<String>): Int {
    require(importIds.isNotEmpty() && importIds.size <= MAX_INTENT_ITEMS) {
      "invalid import acknowledgement count"
    }
    synchronized(lock) {
      val root = inboxRoot(context.applicationContext)
      val receipts = tailLines(File(root, "imports.ndjson"), 1_024).mapNotNullTo(HashSet()) { line ->
        try {
          JSONObject(line).optString("importId").takeIf(importIdPattern::matches)
        } catch (_: Exception) {
          null
        }
      }
      val ledger = File(root, "acknowledged-imports.ndjson")
      val acknowledged = readImportIds(ledger).toMutableSet()
      var removed = 0
      importIds.map(String::lowercase).distinct().forEach { importId ->
        require(importIdPattern.matches(importId) && importId in receipts) {
          "unknown import acknowledgement"
        }
        if (acknowledged.add(importId)) appendLine(ledger, importId)
        removed += cleanupAcknowledgedFile(root, importId)
      }
      return removed
    }
  }

  private fun copyUri(context: Context, uri: Uri, intentToken: String): JSONObject {
    require(uri.scheme == "content") { "only content URIs are accepted" }
    val resolver = context.contentResolver
    val itemToken = IntentToken.from("item", listOf(intentToken, uri.toString()))
    val imports = File(inboxRoot(context), "imports.ndjson")
    tailLines(imports, 1_024).forEach { line ->
      try {
        val existing = JSONObject(line)
        if (existing.optString("itemToken") == itemToken) return existing
      } catch (_: Exception) {
      }
    }
    val declaredMime = resolver.getType(uri)
    val displayName = SafeDisplayName.sanitize(queryDisplayName(context, uri))
    val root = inboxRoot(context)
    val importId = UUID.randomUUID().toString()
    val temporary = File(root, "$importId.part")
    val digest = MessageDigest.getInstance("SHA-256")
    var size = 0L
    val prefix = ArrayList<Byte>(32)
    try {
      resolver.openInputStream(uri).use { input ->
        requireNotNull(input) { "content URI cannot be opened" }
        FileOutputStream(temporary).use { output ->
          val buffer = ByteArray(64 * 1024)
          while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            if (count == 0) continue
            size += count
            require(size <= MAX_IMPORT_BYTES) { "audio import exceeds size limit" }
            repeat(minOf(count, 32 - prefix.size)) { index -> prefix.add(buffer[index]) }
            output.write(buffer, 0, count)
            digest.update(buffer, 0, count)
          }
          output.fd.sync()
        }
      }
      require(size > 0) { "audio import is empty" }
      val media = AudioContent.identify(prefix.toByteArray(), declaredMime)
      validateAudioContainer(temporary)
      val finalFile = File(root, "$importId.${media.extension}")
      check(temporary.renameTo(finalFile)) { "unable to checkpoint imported audio" }
      val metadata = JSONObject()
        .put("importId", importId)
        .put("intentToken", intentToken)
        .put("itemToken", itemToken)
        .put("relativePath", "inbox/${finalFile.name}")
        .put("displayName", displayName)
        .put("mime", media.mimeTypes.first())
        .put("declaredMime", declaredMime ?: JSONObject.NULL)
        .put("sizeBytes", size)
        .put("sha256", digest.digest().joinToString("") { "%02x".format(it) })
        .put("state", "ready")
        .put("receivedAtMs", System.currentTimeMillis())
      appendLine(imports, metadata.toString())
      return metadata
    } catch (error: Exception) {
      temporary.delete()
      throw error
    }
  }

  private fun intentUris(intent: Intent): List<Uri> {
    val values = LinkedHashSet<Uri>()
    intent.clipData?.let { clip ->
      repeat(minOf(clip.itemCount, MAX_INTENT_ITEMS + 1)) { index ->
        clip.getItemAt(index).uri?.let(values::add)
      }
    }
    if (Build.VERSION.SDK_INT >= 33) {
      intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)?.let(values::add)
      intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
        ?.take(MAX_INTENT_ITEMS + 1)
        ?.forEach(values::add)
    } else {
      @Suppress("DEPRECATION")
      (intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))?.let(values::add)
      @Suppress("DEPRECATION")
      intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)
        ?.take(MAX_INTENT_ITEMS + 1)
        ?.forEach(values::add)
    }
    return values.toList()
  }

  private fun inboxRoot(context: Context): File {
    val root = File(context.filesDir, "inbox")
    check(root.mkdirs() || root.isDirectory)
    return root
  }

  private fun queryDisplayName(context: Context, uri: Uri): String? {
    return try {
      context.contentResolver.query(
        uri,
        arrayOf(OpenableColumns.DISPLAY_NAME),
        null,
        null,
        null,
      )?.use { cursor ->
        if (cursor.moveToFirst()) cursor.getString(0) else null
      }
    } catch (_: Exception) {
      null
    }
  }

  private fun validateAudioContainer(file: File) {
    val extractor = MediaExtractor()
    try {
      extractor.setDataSource(file.absolutePath)
      require(extractor.trackCount in 1..32) { "audio track count is invalid" }
      repeat(extractor.trackCount) { index ->
        val mime = extractor.getTrackFormat(index).getString(MediaFormat.KEY_MIME)
        require(mime != null && mime.startsWith("audio/")) {
          "import contains a non-audio track"
        }
      }
    } finally {
      extractor.release()
    }
  }

  private fun readTokens(file: File): Set<String> {
    if (!file.isFile) return emptySet()
    return tailLines(file, 1_024)
      .filterTo(HashSet()) { it.matches(Regex("[0-9a-f]{64}")) }
  }

  private fun readImportIds(file: File): Set<String> {
    if (!file.isFile) return emptySet()
    return tailLines(file, 1_024).filterTo(HashSet(), importIdPattern::matches)
  }

  private fun cleanupAcknowledgedFile(root: File, importId: String): Int {
    var removed = 0
    supportedExtensions.forEach { suffix ->
      val candidate = File(root, "$importId.$suffix")
      if (candidate.isFile && candidate.delete()) removed += 1
    }
    return removed
  }

  private fun tailLines(file: File, maximum: Int): List<String> {
    if (!file.isFile) return emptyList()
    val values = java.util.ArrayDeque<String>(maximum)
    file.bufferedReader(Charsets.UTF_8).useLines { lines ->
      lines.forEach { line ->
        if (values.size == maximum) values.removeFirst()
        values.addLast(line)
      }
    }
    return values.toList()
  }

  private fun appendLine(file: File, value: String) {
    FileOutputStream(file, true).use { output ->
      output.write((value + "\n").toByteArray(Charsets.UTF_8))
      output.fd.sync()
    }
  }
}
