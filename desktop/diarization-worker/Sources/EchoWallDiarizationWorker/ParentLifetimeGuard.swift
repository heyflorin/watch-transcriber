import Darwin
import Foundation

enum ParentLifetimeError: Error {
  case invalidIdentity
  case identityMismatch

  var safeCode: String {
    switch self {
    case .invalidIdentity: "invalid_parent_identity"
    case .identityMismatch: "parent_identity_mismatch"
    }
  }
}

// This optional binding is supplied only by the App's cleared environment.
// The worker never signals a supplied PID: it exits itself when its real
// kernel parent changes, so a reused PID cannot become its new owner.
final class ParentLifetimeGuard {
  private final class Monitor: @unchecked Sendable {
    let parent: pid_t
    let lock = NSLock()
    let changed = DispatchSemaphore(value: 0)
    let finished = DispatchGroup()
    private var stopped = false

    init(parent: pid_t) { self.parent = parent }

    func run() {
      defer { finished.leave() }
      while true {
        lock.lock()
        let shouldStop = stopped
        lock.unlock()
        if shouldStop { return }
        if getppid() != parent { _exit(74) }
        // DispatchTime is monotonic; a wall-clock correction must not delay
        // detection of a dead owner.
        _ = changed.wait(timeout: .now() + .milliseconds(100))
      }
    }

    func stop() {
      lock.lock()
      stopped = true
      lock.unlock()
      changed.signal()
      finished.wait()
    }
  }

  private let monitor: Monitor

  static func bindFromEnvironment() throws -> ParentLifetimeGuard? {
    guard let supplied = ProcessInfo.processInfo.environment["ECHOWALL_WORKER_PARENT_PID"] else {
      return nil  // Standalone one-shot CLI callers retain their existing contract.
    }
    guard let parent = Int32(supplied), parent > 1, String(parent) == supplied else {
      throw ParentLifetimeError.invalidIdentity
    }
    guard getppid() == parent else { throw ParentLifetimeError.identityMismatch }
    return ParentLifetimeGuard(parent: parent)
  }

  private init(parent: pid_t) {
    let monitor = Monitor(parent: parent)
    self.monitor = monitor
    monitor.finished.enter()
    let thread = Thread { monitor.run() }
    thread.name = "worker-parent-lifetime"
    thread.qualityOfService = .utility
    thread.start()
  }

  func stop() { monitor.stop() }
  deinit { monitor.stop() }
}
