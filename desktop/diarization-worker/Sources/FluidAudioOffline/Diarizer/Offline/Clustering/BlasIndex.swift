import Accelerate

typealias BlasIndex = Int32

@inline(__always)
func makeBlasIndex(_ value: Int, label: String) throws -> BlasIndex {
  guard let cast = BlasIndex(exactly: value) else {
    throw OfflineDiarizationError.processingFailed("\(label) exceeds supported range")
  }
  return cast
}
