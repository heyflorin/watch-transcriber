package ai.ax.watch_transcriber

import android.os.Bundle
import android.content.Intent
import androidx.activity.enableEdgeToEdge
import ai.ax.watch_transcriber.capture.ImportInbox
import ai.ax.watch_transcriber.capture.RecordingService
import ai.ax.watch_transcriber.capture.SessionJournal
import io.crates.keyring.Keyring

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    // Must run before super.onCreate() starts Rust: the sync core reads the
    // Keystore-backed credential store during app setup.
    Keyring.initializeNdkContext(applicationContext)
    super.onCreate(savedInstanceState)
    if (!RecordingService.isActive()) SessionJournal(this).recoverInterrupted()
    ImportInbox.consumeIntentAsync(this, intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    ImportInbox.consumeIntentAsync(this, intent)
  }
}
