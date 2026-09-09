import UIKit
import UniformTypeIdentifiers

final class ShareViewController: UIViewController {
  private static let appGroup = "group.ai.ax.watch-transcriber"
  private static let maximumBytes: Int64 = 512 * 1024 * 1024
  private static let supportedExtensions = Set(["m4a", "mp3", "wav"])
  private var started = false
  private let status = UILabel()

  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = .systemBackground
    status.text = "正在保存到 EchoWall…"
    status.textAlignment = .center
    status.numberOfLines = 0
    status.translatesAutoresizingMaskIntoConstraints = false
    view.addSubview(status)
    NSLayoutConstraint.activate([
      status.centerXAnchor.constraint(equalTo: view.centerXAnchor),
      status.centerYAnchor.constraint(equalTo: view.centerYAnchor),
      status.leadingAnchor.constraint(greaterThanOrEqualTo: view.leadingAnchor, constant: 24),
      status.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
    ])
  }

  override func viewDidAppear(_ animated: Bool) {
    super.viewDidAppear(animated)
    guard !started else { return }
    started = true
    importAttachments()
  }

  private func importAttachments() {
    let providers = (extensionContext?.inputItems as? [NSExtensionItem] ?? [])
      .flatMap { $0.attachments ?? [] }
      .filter { $0.hasItemConformingToTypeIdentifier(UTType.audio.identifier) }
    guard !providers.isEmpty, providers.count <= 16 else {
      finish(message: "请选择 1–16 个音频文件", error: true)
      return
    }
    let group = DispatchGroup()
    let resultQueue = DispatchQueue(label: "ai.ax.echowall.share-results")
    var accepted = 0
    for provider in providers {
      group.enter()
      provider.loadFileRepresentation(forTypeIdentifier: UTType.audio.identifier) {
        [weak self] url, _ in
        defer { group.leave() }
        guard let self, let url else { return }
        do {
          try self.copyIntoSharedInbox(url)
          resultQueue.sync { accepted += 1 }
        } catch {
          // Each item is independent. The final count states exactly how many
          // durable copies succeeded without exposing private filenames.
        }
      }
    }
    group.notify(queue: .main) { [weak self] in
      guard let self else { return }
      if accepted == providers.count {
        self.finish(message: "已发送到 EchoWall", error: false)
      } else if accepted > 0 {
        self.finish(message: "已保存 \(accepted) 个；其余文件不受支持", error: false)
      } else {
        self.finish(message: "音频未保存，请回到 EchoWall 重试", error: true)
      }
    }
  }

  private func copyIntoSharedInbox(_ source: URL) throws {
    guard source.isFileURL,
          let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: Self.appGroup
          ) else { throw ShareFailure.unavailable }
    let values = try source.resourceValues(forKeys: [
      .isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey,
    ])
    guard values.isRegularFile == true,
          values.isSymbolicLink != true,
          let size = values.fileSize,
          size > 0,
          Int64(size) <= Self.maximumBytes else { throw ShareFailure.invalidFile }
    let fileExtension = source.pathExtension.lowercased()
    guard Self.supportedExtensions.contains(fileExtension) else {
      throw ShareFailure.unsupported
    }
    let inbox = container.appendingPathComponent("share-inbox", isDirectory: true)
    try FileManager.default.createDirectory(
      at: inbox, withIntermediateDirectories: true
    )
    let identifier = UUID().uuidString.lowercased()
    let temporary = inbox.appendingPathComponent("\(identifier).part")
    let destination = inbox.appendingPathComponent("\(identifier).\(fileExtension)")
    try FileManager.default.copyItem(at: source, to: temporary)
    do {
      let copied = try temporary.resourceValues(forKeys: [.fileSizeKey]).fileSize
      guard copied == size else { throw ShareFailure.sourceChanged }
      let handle = try FileHandle(forWritingTo: temporary)
      handle.synchronizeFile()
      handle.closeFile()
      try FileManager.default.moveItem(at: temporary, to: destination)
      try writeReceipt(
        inbox: inbox,
        id: identifier,
        displayName: safeDisplayName(source.lastPathComponent, fallback: destination.lastPathComponent),
        size: size
      )
    } catch {
      try? FileManager.default.removeItem(at: temporary)
      try? FileManager.default.removeItem(at: destination)
      try? FileManager.default.removeItem(
        at: inbox.appendingPathComponent("\(identifier).json")
      )
      throw error
    }
  }

  private func writeReceipt(
    inbox: URL, id: String, displayName: String, size: Int
  ) throws {
    let receipt: [String: Any] = [
      "schema_version": 1,
      "import_id": id,
      "display_name": displayName,
      "size_bytes": size,
      "received_at": ISO8601DateFormatter().string(from: Date()),
    ]
    let data = try JSONSerialization.data(withJSONObject: receipt, options: [.sortedKeys])
    try data.write(
      to: inbox.appendingPathComponent("\(id).json"), options: [.atomic]
    )
  }

  private func safeDisplayName(_ value: String, fallback: String) -> String {
    let scalars = value.unicodeScalars.filter {
      $0.value >= 32 && $0 != "/" && $0 != "\\"
    }
    let cleaned = String(String.UnicodeScalarView(scalars.prefix(160)))
      .trimmingCharacters(in: .whitespacesAndNewlines)
    return cleaned.isEmpty ? fallback : cleaned
  }

  private func finish(message: String, error: Bool) {
    status.text = message
    status.textColor = error ? .systemRed : .label
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.7) { [weak self] in
      self?.extensionContext?.completeRequest(returningItems: nil)
    }
  }
}

private enum ShareFailure: Error {
  case unavailable
  case invalidFile
  case unsupported
  case sourceChanged
}
