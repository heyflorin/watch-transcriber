use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use sha2::{Digest, Sha256};
use windows::core::{implement, Interface, PCWSTR};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    eCapture, eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::StructuredStorage::{
    InitPropVariantFromBuffer, PropVariantClear, PropVariantToStringAlloc,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::capture::model::{
    CapturePlan, CapturePreflight, CaptureTarget, MeetingSource, PermissionState,
    SourceAvailability,
};
use crate::capture::session::{
    CaptureError, CaptureStartContext, DesktopCaptureBackend, NativeCaptureSignal,
    NativeCaptureStream, SegmentCloseReason, SegmentMetadata, ROLLING_SEGMENT_NS,
};
use crate::ingest::envelope::{Platform, TrackRole};

use super::{
    ClockMapper, PacketFlags, PacketIssue, PacketTimeline, WasapiPacket,
    MICROPHONE_PRIVACY_SETTINGS_URI,
};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const BITS_PER_SAMPLE: u16 = 16;
const BLOCK_ALIGN: u16 = CHANNELS * (BITS_PER_SAMPLE / 8);
const EVENT_WAIT_MS: u32 = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowsCaptureSource {
    Endpoint {
        id: String,
        label: String,
        is_microphone: bool,
    },
    Process {
        id: String,
        pid: u32,
        label: String,
        browser: bool,
    },
}

#[derive(Debug, Default)]
pub struct WindowsCaptureBackend;

impl WindowsCaptureBackend {
    pub fn enumerate_sources() -> Result<Vec<WindowsCaptureSource>, CaptureError> {
        let _com = ComApartment::initialize()?;
        let enumerator = endpoint_enumerator()?;
        let mut sources = enumerate_endpoints(&enumerator, eCapture, true)?;
        sources.extend(enumerate_endpoints(&enumerator, eRender, false)?);
        sources.extend(enumerate_processes()?);
        Ok(sources)
    }
}

impl DesktopCaptureBackend for WindowsCaptureBackend {
    fn preflight(&self, plan: &CapturePlan) -> Result<CapturePreflight, CaptureError> {
        plan.validate()?;
        if plan.platform != Platform::Windows {
            return Ok(unavailable_preflight(
                "Windows WASAPI backend requires a Windows capture plan",
            ));
        }
        let sources = Self::enumerate_sources()?;
        if matches!(plan.target, CaptureTarget::Meeting { .. })
            && windows_build_number()? < super::PROCESS_LOOPBACK_MINIMUM_BUILD
        {
            return Ok(CapturePreflight {
                backend_available: true,
                microphone_permission: PermissionState::Granted,
                system_audio_permission: PermissionState::Unavailable,
                selected_source: SourceAvailability::Unavailable,
                warnings: vec![format!(
                    "Process-loopback requires Windows build {} or later",
                    super::PROCESS_LOOPBACK_MINIMUM_BUILD
                )],
            });
        }
        let microphone_id = match &plan.target {
            CaptureTarget::VoiceMemo { microphone }
            | CaptureTarget::Meeting { microphone, .. }
            | CaptureTarget::SystemCapture { microphone, .. } => &microphone.id,
        };
        let microphone_available = sources.iter().any(|source| {
            matches!(source, WindowsCaptureSource::Endpoint { id, is_microphone: true, .. } if id == microphone_id)
        });
        if !microphone_available {
            return Ok(CapturePreflight {
                backend_available: true,
                microphone_permission: PermissionState::Unavailable,
                system_audio_permission: PermissionState::NotRequired,
                selected_source: SourceAvailability::Missing,
                warnings: vec![format!(
                    "Selected microphone is unavailable; review {MICROPHONE_PRIVACY_SETTINGS_URI}"
                )],
            });
        }
        let _com = ComApartment::initialize()?;
        match WasapiAudioClient::activate(&TrackConfiguration::Microphone(microphone_id.clone())) {
            Ok(probe) => drop(probe),
            Err(error) if error.code == "microphone_privacy_denied" => {
                return Ok(CapturePreflight {
                    backend_available: true,
                    microphone_permission: PermissionState::Denied,
                    system_audio_permission: PermissionState::NotRequired,
                    selected_source: SourceAvailability::Available,
                    warnings: vec![format!(
                        "Microphone access is denied; open {MICROPHONE_PRIVACY_SETTINGS_URI}"
                    )],
                });
            }
            Err(_) => {
                return Ok(CapturePreflight {
                    backend_available: true,
                    microphone_permission: PermissionState::Unavailable,
                    system_audio_permission: PermissionState::NotRequired,
                    selected_source: SourceAvailability::Available,
                    warnings: vec![
                        "Selected microphone could not be initialized in shared event mode"
                            .to_owned(),
                    ],
                });
            }
        }

        let (system_permission, selected_source, warnings) = match &plan.target {
            CaptureTarget::VoiceMemo { .. } => (
                PermissionState::NotRequired,
                SourceAvailability::Available,
                Vec::new(),
            ),
            CaptureTarget::SystemCapture { output, .. } => {
                let found = sources.iter().any(|source| {
                    matches!(source, WindowsCaptureSource::Endpoint { id, is_microphone: false, .. } if id == &output.id)
                });
                (
                    PermissionState::Granted,
                    if found {
                        SourceAvailability::Available
                    } else {
                        SourceAvailability::Missing
                    },
                    vec!["Protected/DRM audio is unsupported".to_owned()],
                )
            }
            CaptureTarget::Meeting { source, .. } => {
                let id = match source {
                    MeetingSource::NativeApplication { id, .. }
                    | MeetingSource::WholeBrowser { id, .. } => id,
                };
                let found = selected_process_id(id).is_ok()
                    && sources.iter().any(|candidate| match (source, candidate) {
                        (
                            MeetingSource::WholeBrowser { .. },
                            WindowsCaptureSource::Process {
                                id: candidate_id,
                                browser: true,
                                ..
                            },
                        ) => candidate_id == id,
                        (
                            MeetingSource::NativeApplication { .. },
                            WindowsCaptureSource::Process {
                                id: candidate_id,
                                browser: false,
                                ..
                            },
                        ) => candidate_id == id,
                        _ => false,
                    });
                let warnings = if matches!(source, MeetingSource::WholeBrowser { .. }) {
                    vec![
                        "Windows browser capture includes the selected browser process tree and may include other tabs; it never falls back to all-system audio"
                            .to_owned(),
                        "Protected/DRM audio is unsupported".to_owned(),
                    ]
                } else {
                    vec!["Protected/DRM audio is unsupported".to_owned()]
                };
                (
                    PermissionState::Granted,
                    if found {
                        SourceAvailability::Available
                    } else {
                        SourceAvailability::Missing
                    },
                    warnings,
                )
            }
        };
        Ok(CapturePreflight {
            backend_available: true,
            microphone_permission: PermissionState::Granted,
            system_audio_permission: system_permission,
            selected_source,
            warnings,
        })
    }

    fn start(
        &self,
        context: CaptureStartContext,
    ) -> Result<Box<dyn NativeCaptureStream>, CaptureError> {
        let preflight = self.preflight(&context.plan)?;
        if !preflight.ready_for(&context.plan) {
            return Err(CaptureError::local(
                "windows_preflight_not_ready",
                "Windows capture source or microphone permission is unavailable",
            ));
        }
        WindowsNativeStream::start(context).map(|stream| Box::new(stream) as _)
    }
}

struct WindowsNativeStream {
    tracks: Vec<TrackController>,
    signals: Arc<Mutex<Vec<NativeCaptureSignal>>>,
}

impl WindowsNativeStream {
    fn start(context: CaptureStartContext) -> Result<Self, CaptureError> {
        let signals = Arc::new(Mutex::new(Vec::new()));
        let microphone_id = match &context.plan.target {
            CaptureTarget::VoiceMemo { microphone }
            | CaptureTarget::Meeting { microphone, .. }
            | CaptureTarget::SystemCapture { microphone, .. } => microphone.id.clone(),
        };
        let mut tracks = vec![TrackController::spawn(
            TrackConfiguration::Microphone(microphone_id),
            &context,
            Arc::clone(&signals),
        )?];
        let system_configuration = match &context.plan.target {
            CaptureTarget::VoiceMemo { .. } => None,
            CaptureTarget::SystemCapture { output, .. } => {
                Some(TrackConfiguration::EndpointLoopback(output.id.clone()))
            }
            CaptureTarget::Meeting { source, .. } => {
                let id = match source {
                    MeetingSource::NativeApplication { id, .. }
                    | MeetingSource::WholeBrowser { id, .. } => id,
                };
                Some(TrackConfiguration::ProcessLoopback(selected_process_id(
                    id,
                )?))
            }
        };
        if let Some(configuration) = system_configuration {
            match TrackController::spawn(configuration, &context, Arc::clone(&signals)) {
                Ok(track) => tracks.push(track),
                Err(error) => {
                    for track in &mut tracks {
                        track.shutdown();
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self { tracks, signals })
    }
}

impl NativeCaptureStream for WindowsNativeStream {
    fn close_segment(
        &mut self,
        reason: SegmentCloseReason,
        clock_at_ns: u64,
    ) -> Result<Vec<SegmentMetadata>, CaptureError> {
        let mut closed = Vec::with_capacity(self.tracks.len());
        for track in &mut self.tracks {
            closed.push(track.close(reason, clock_at_ns)?);
        }
        Ok(closed)
    }

    fn resume(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        for track in &mut self.tracks {
            track.resume(clock_at_ns)?;
        }
        Ok(())
    }

    fn take_signals(&mut self) -> Result<Vec<NativeCaptureSignal>, CaptureError> {
        let mut signals = self.signals.lock().map_err(|_| {
            CaptureError::local(
                "windows_capture_failed",
                "Windows capture signal queue is unavailable",
            )
        })?;
        let mut drained = std::mem::take(&mut *signals);
        drained.sort_by_key(signal_clock);
        drained.dedup();
        Ok(drained)
    }
}

impl Drop for WindowsNativeStream {
    fn drop(&mut self) {
        for track in &mut self.tracks {
            track.shutdown();
        }
    }
}

fn signal_clock(signal: &NativeCaptureSignal) -> u64 {
    match signal {
        NativeCaptureSignal::Gap { clock_start_ns, .. } => *clock_start_ns,
        NativeCaptureSignal::SourceLost { clock_at_ns, .. } => *clock_at_ns,
    }
}

#[derive(Debug, Clone)]
enum TrackConfiguration {
    Microphone(String),
    EndpointLoopback(String),
    ProcessLoopback(u32),
}

impl TrackConfiguration {
    fn role(&self) -> TrackRole {
        match self {
            Self::Microphone(_) => TrackRole::Microphone,
            Self::EndpointLoopback(_) | Self::ProcessLoopback(_) => TrackRole::System,
        }
    }
}

enum TrackCommand {
    Close {
        reason: SegmentCloseReason,
        clock_at_ns: u64,
        response: SyncSender<Result<SegmentMetadata, CaptureError>>,
    },
    Resume {
        clock_at_ns: u64,
        response: SyncSender<Result<(), CaptureError>>,
    },
    Shutdown,
}

struct TrackController {
    sender: Sender<TrackCommand>,
    worker: Option<JoinHandle<()>>,
}

impl TrackController {
    fn spawn(
        configuration: TrackConfiguration,
        context: &CaptureStartContext,
        signals: Arc<Mutex<Vec<NativeCaptureSignal>>>,
    ) -> Result<Self, CaptureError> {
        let (sender, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let package = context.package_directory.clone();
        let start_ns = context.monotonic_started_ns;
        let worker = thread::Builder::new()
            .name(format!("wasapi-{}", role_name(configuration.role())))
            .spawn(move || {
                let result = TrackWorker::new(configuration, package, start_ns, signals);
                match result {
                    Ok(mut worker) => {
                        let _ = ready_sender.send(Ok(()));
                        worker.run(receiver);
                    }
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                    }
                }
            })
            .map_err(|_| {
                CaptureError::local(
                    "windows_capture_failed",
                    "failed to create Windows capture worker",
                )
            })?;
        match ready_receiver.recv().map_err(|_| {
            CaptureError::local(
                "windows_capture_failed",
                "Windows capture worker ended during initialization",
            )
        })? {
            Ok(()) => Ok(Self {
                sender,
                worker: Some(worker),
            }),
            Err(error) => {
                let _ = worker.join();
                Err(error)
            }
        }
    }

    fn close(
        &mut self,
        reason: SegmentCloseReason,
        clock_at_ns: u64,
    ) -> Result<SegmentMetadata, CaptureError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(TrackCommand::Close {
                reason,
                clock_at_ns,
                response: sender,
            })
            .map_err(|_| worker_ended())?;
        receiver.recv().map_err(|_| worker_ended())?
    }

    fn resume(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(TrackCommand::Resume {
                clock_at_ns,
                response: sender,
            })
            .map_err(|_| worker_ended())?;
        receiver.recv().map_err(|_| worker_ended())?
    }

    fn shutdown(&mut self) {
        let _ = self.sender.send(TrackCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_ended() -> CaptureError {
    CaptureError::local(
        "windows_capture_failed",
        "Windows capture worker ended unexpectedly",
    )
}

struct TrackWorker {
    _com: ComApartment,
    configuration: TrackConfiguration,
    audio: WasapiAudioClient,
    writer: Option<PcmWavWriter>,
    sequence: u32,
    package: PathBuf,
    mapper: ClockMapper,
    timeline: PacketTimeline,
    signals: Arc<Mutex<Vec<NativeCaptureSignal>>>,
    source_exit_reported: bool,
}

impl TrackWorker {
    fn new(
        configuration: TrackConfiguration,
        package: PathBuf,
        start_ns: u64,
        signals: Arc<Mutex<Vec<NativeCaptureSignal>>>,
    ) -> Result<Self, CaptureError> {
        let com = ComApartment::initialize()?;
        let qpc_origin = current_qpc_100ns()?;
        let audio = WasapiAudioClient::activate(&configuration)?;
        let writer = PcmWavWriter::create(&package, configuration.role(), 0, start_ns)?;
        audio.start()?;
        Ok(Self {
            _com: com,
            configuration,
            audio,
            writer: Some(writer),
            sequence: 0,
            package,
            mapper: ClockMapper::new(qpc_origin, start_ns),
            timeline: PacketTimeline::new(ClockMapper::new(qpc_origin, start_ns), SAMPLE_RATE)?,
            signals,
            source_exit_reported: false,
        })
    }

    fn run(&mut self, receiver: Receiver<TrackCommand>) {
        loop {
            match receiver.try_recv() {
                Ok(command) => {
                    if self.handle(command) {
                        return;
                    }
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match self.audio.wait() {
                Ok(true) => {
                    if let Err(error) = self.drain_packets() {
                        self.push_source_loss("wasapi_stream_error", error.message);
                    }
                }
                Ok(false) => self.check_process_exit(),
                Err(error) => self.push_source_loss("wasapi_device_invalidated", error.message),
            }
        }
    }

    fn handle(&mut self, command: TrackCommand) -> bool {
        match command {
            TrackCommand::Close {
                reason,
                clock_at_ns,
                response,
            } => {
                let result = self.close_segment(reason, clock_at_ns);
                let _ = response.send(result);
                false
            }
            TrackCommand::Resume {
                clock_at_ns,
                response,
            } => {
                let result = self.resume(clock_at_ns);
                let _ = response.send(result);
                false
            }
            TrackCommand::Shutdown => {
                let _ = self.audio.stop();
                true
            }
        }
    }

    fn drain_packets(&mut self) -> Result<(), CaptureError> {
        while self.audio.next_packet_frames()? > 0 {
            let packet = self.audio.read_packet()?;
            let analysis = self.timeline.observe(&packet.metadata)?;
            for issue in &analysis.issues {
                self.report_issue(*issue, analysis.clock_start_ns, analysis.clock_end_ns);
            }
            if let Some(writer) = self.writer.as_mut() {
                writer.write_packet(
                    &packet.bytes,
                    packet.metadata.frames,
                    analysis.silent,
                    analysis.clock_start_ns,
                )?;
            }
            self.audio.release_packet(packet.metadata.frames)?;
        }
        Ok(())
    }

    fn report_issue(&self, issue: PacketIssue, start_ns: u64, _end_ns: u64) {
        let (gap_start, gap_end, code, message) = match issue {
            PacketIssue::DataDiscontinuity { .. } => (
                start_ns.saturating_sub(1),
                start_ns,
                "wasapi_data_discontinuity",
                "WASAPI reported a device-position discontinuity",
            ),
            PacketIssue::TimestampError => (
                start_ns.saturating_sub(1),
                start_ns,
                "wasapi_timestamp_error",
                "WASAPI reported an unreliable packet timestamp",
            ),
            PacketIssue::ClockDrift {
                expected_clock_ns,
                actual_clock_ns,
                ..
            } => (
                expected_clock_ns.min(actual_clock_ns),
                expected_clock_ns.max(actual_clock_ns),
                "wasapi_clock_drift",
                "WASAPI packet clock drift exceeded the supported tolerance",
            ),
        };
        let signal = NativeCaptureSignal::Gap {
            clock_start_ns: gap_start,
            clock_end_ns: gap_end.max(gap_start.saturating_add(1)),
            code,
            message: message.to_owned(),
        };
        if let Ok(mut signals) = self.signals.lock() {
            signals.push(signal);
        }
    }

    fn check_process_exit(&mut self) {
        let TrackConfiguration::ProcessLoopback(pid) = self.configuration else {
            return;
        };
        if !self.source_exit_reported && !process_exists(pid).unwrap_or(true) {
            self.push_source_loss(
                "wasapi_source_exited",
                "Selected meeting process tree exited".to_owned(),
            );
        }
    }

    fn push_source_loss(&mut self, code: &'static str, message: String) {
        if self.source_exit_reported {
            return;
        }
        self.source_exit_reported = true;
        let clock_at_ns = current_qpc_100ns()
            .ok()
            .and_then(|qpc| self.mapper.map(qpc).ok())
            .unwrap_or_else(|| self.writer.as_ref().map_or(0, |writer| writer.start_ns));
        if let Ok(mut signals) = self.signals.lock() {
            signals.push(NativeCaptureSignal::SourceLost {
                clock_at_ns,
                code,
                message,
            });
        }
    }

    fn close_segment(
        &mut self,
        reason: SegmentCloseReason,
        clock_at_ns: u64,
    ) -> Result<SegmentMetadata, CaptureError> {
        let _ = self.drain_packets();
        if !matches!(reason, SegmentCloseReason::RollingLimit) {
            let stopped = self.audio.stop();
            if !matches!(reason, SegmentCloseReason::SourceLoss) {
                stopped?;
            }
        }
        let writer = self.writer.take().ok_or_else(|| {
            CaptureError::local(
                "windows_capture_failed",
                "Windows capture track has no active segment",
            )
        })?;
        let metadata = writer.finalize(clock_at_ns)?;
        if matches!(reason, SegmentCloseReason::RollingLimit) {
            self.sequence = self.sequence.saturating_add(1);
            self.writer = Some(PcmWavWriter::create(
                &self.package,
                self.configuration.role(),
                self.sequence,
                clock_at_ns,
            )?);
        }
        Ok(metadata)
    }

    fn resume(&mut self, clock_at_ns: u64) -> Result<(), CaptureError> {
        if self.writer.is_some() {
            return Err(CaptureError::local(
                "windows_capture_failed",
                "Windows capture track is already active",
            ));
        }
        self.sequence = self.sequence.saturating_add(1);
        self.writer = Some(PcmWavWriter::create(
            &self.package,
            self.configuration.role(),
            self.sequence,
            clock_at_ns,
        )?);
        if self.source_exit_reported {
            self.audio = WasapiAudioClient::activate(&self.configuration)?;
        }
        self.timeline = PacketTimeline::new(self.mapper, SAMPLE_RATE)?;
        self.source_exit_reported = false;
        self.audio.start()
    }
}

struct CapturedPacket {
    metadata: WasapiPacket,
    bytes: Vec<u8>,
}

struct WasapiAudioClient {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

impl WasapiAudioClient {
    fn activate(configuration: &TrackConfiguration) -> Result<Self, CaptureError> {
        let client = match configuration {
            TrackConfiguration::Microphone(id) => activate_endpoint(id)?,
            TrackConfiguration::EndpointLoopback(id) => activate_endpoint(id)?,
            TrackConfiguration::ProcessLoopback(pid) => activate_process_loopback(*pid)?,
        };
        let format = pcm_format();
        let loopback = !matches!(configuration, TrackConfiguration::Microphone(_));
        let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        if loopback {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        unsafe {
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    flags,
                    10_000_000,
                    0,
                    &format,
                    None,
                )
                .map_err(map_wasapi_error)?;
            let event =
                CreateEventW(None, false, false, PCWSTR::null()).map_err(map_windows_error)?;
            if let Err(error) = client.SetEventHandle(event) {
                let _ = CloseHandle(event);
                return Err(map_windows_error(error));
            }
            let capture = client
                .GetService::<IAudioCaptureClient>()
                .map_err(map_wasapi_error)?;
            Ok(Self {
                client,
                capture,
                event,
            })
        }
    }

    fn start(&self) -> Result<(), CaptureError> {
        unsafe { self.client.Start().map_err(map_wasapi_error) }
    }

    fn stop(&self) -> Result<(), CaptureError> {
        unsafe { self.client.Stop().map_err(map_wasapi_error) }
    }

    fn wait(&self) -> Result<bool, CaptureError> {
        match unsafe { WaitForSingleObject(self.event, EVENT_WAIT_MS) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(CaptureError::local(
                "windows_capture_failed",
                "Windows audio event wait failed",
            )),
        }
    }

    fn next_packet_frames(&self) -> Result<u32, CaptureError> {
        unsafe { self.capture.GetNextPacketSize().map_err(map_wasapi_error) }
    }

    fn read_packet(&self) -> Result<CapturedPacket, CaptureError> {
        let mut data = std::ptr::null_mut();
        let mut frames = 0;
        let mut flags = 0;
        let mut device_position = 0;
        let mut qpc_100ns = 0;
        unsafe {
            self.capture
                .GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut device_position),
                    Some(&mut qpc_100ns),
                )
                .map_err(map_wasapi_error)?;
        }
        let byte_count = usize::try_from(frames)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(BLOCK_ALIGN)))
            .ok_or_else(|| {
                CaptureError::local(
                    "invalid_wasapi_packet",
                    "WASAPI packet size exceeds the supported range",
                )
            })?;
        let bytes = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
            vec![0; byte_count]
        } else if data.is_null() {
            return Err(CaptureError::local(
                "invalid_wasapi_packet",
                "WASAPI returned a null packet buffer",
            ));
        } else {
            unsafe { std::slice::from_raw_parts(data, byte_count).to_vec() }
        };
        Ok(CapturedPacket {
            metadata: WasapiPacket {
                frames,
                device_position,
                qpc_100ns,
                flags: PacketFlags::from_bits(flags),
            },
            bytes,
        })
    }

    fn release_packet(&self, frames: u32) -> Result<(), CaptureError> {
        unsafe { self.capture.ReleaseBuffer(frames).map_err(map_wasapi_error) }
    }
}

impl Drop for WasapiAudioClient {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

struct PcmWavWriter {
    file: File,
    temp_path: PathBuf,
    final_path: PathBuf,
    relative_path: String,
    role: TrackRole,
    sequence: u32,
    start_ns: u64,
    frames_written: u64,
}

impl PcmWavWriter {
    fn create(
        package: &Path,
        role: TrackRole,
        sequence: u32,
        start_ns: u64,
    ) -> Result<Self, CaptureError> {
        let filename = format!("{}-{sequence:05}.wav", role_name(role));
        let relative_path = format!("tracks/{filename}");
        let final_path = package.join(&relative_path);
        let temp_path = package.join("tracks").join(format!(".{filename}.tmp"));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&temp_path)?;
        file.write_all(&[0; 44])?;
        Ok(Self {
            file,
            temp_path,
            final_path,
            relative_path,
            role,
            sequence,
            start_ns,
            frames_written: 0,
        })
    }

    fn write_packet(
        &mut self,
        bytes: &[u8],
        frames: u32,
        silent: bool,
        clock_start_ns: u64,
    ) -> Result<(), CaptureError> {
        let expected = usize::try_from(frames)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(BLOCK_ALIGN)))
            .ok_or_else(|| {
                CaptureError::local("invalid_wasapi_packet", "WASAPI packet size is invalid")
            })?;
        if bytes.len() != expected {
            return Err(CaptureError::local(
                "invalid_wasapi_packet",
                "WASAPI packet byte count does not match its frame count",
            ));
        }
        let target_frame = clock_start_ns
            .saturating_sub(self.start_ns)
            .saturating_mul(u64::from(SAMPLE_RATE))
            / 1_000_000_000;
        if target_frame > self.frames_written {
            let missing_frames = target_frame - self.frames_written;
            let missing_bytes = usize::try_from(missing_frames)
                .ok()
                .and_then(|frames| frames.checked_mul(usize::from(BLOCK_ALIGN)))
                .ok_or_else(|| {
                    CaptureError::local(
                        "invalid_wasapi_packet",
                        "WASAPI packet gap exceeds the supported range",
                    )
                })?;
            write_zeros(&mut self.file, missing_bytes)?;
            self.frames_written = target_frame;
        }
        let overlapped_frames = self.frames_written.saturating_sub(target_frame);
        if overlapped_frames >= u64::from(frames) {
            return Ok(());
        }
        let skipped_bytes = usize::try_from(overlapped_frames)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(BLOCK_ALIGN)))
            .ok_or_else(|| {
                CaptureError::local(
                    "invalid_wasapi_packet",
                    "WASAPI packet overlap exceeds the supported range",
                )
            })?;
        let bytes = &bytes[skipped_bytes..];
        let frames = u64::from(frames) - overlapped_frames;
        if silent {
            write_zeros(&mut self.file, bytes.len())?;
        } else {
            self.file.write_all(bytes)?;
        }
        self.frames_written = self.frames_written.saturating_add(frames);
        Ok(())
    }

    fn finalize(mut self, clock_end_ns: u64) -> Result<SegmentMetadata, CaptureError> {
        if clock_end_ns <= self.start_ns || clock_end_ns - self.start_ns > ROLLING_SEGMENT_NS {
            return Err(CaptureError::local(
                "invalid_segment",
                "Windows WAV segment exceeded its five-minute clock boundary",
            ));
        }
        let target_frames =
            (clock_end_ns - self.start_ns).saturating_mul(u64::from(SAMPLE_RATE)) / 1_000_000_000;
        if target_frames == 0 {
            return Err(CaptureError::local(
                "invalid_segment",
                "Windows WAV segment contains no complete audio frame",
            ));
        }
        if self.frames_written < target_frames {
            let missing_frames = target_frames - self.frames_written;
            let missing_bytes = usize::try_from(missing_frames)
                .ok()
                .and_then(|frames| frames.checked_mul(usize::from(BLOCK_ALIGN)))
                .ok_or_else(|| {
                    CaptureError::local(
                        "invalid_segment",
                        "Windows WAV segment size exceeds the supported range",
                    )
                })?;
            write_zeros(&mut self.file, missing_bytes)?;
        } else if self.frames_written > target_frames {
            self.file
                .set_len(44 + target_frames.saturating_mul(u64::from(BLOCK_ALIGN)))?;
        }
        self.frames_written = target_frames;
        write_wav_header(&mut self.file, self.frames_written)?;
        self.file.sync_all()?;
        drop(self.file);
        fs::rename(&self.temp_path, &self.final_path)?;
        if let Some(parent) = self.final_path.parent() {
            File::open(parent)?.sync_all()?;
        }
        let sha256 = hash_file(&self.final_path)?;
        Ok(SegmentMetadata {
            role: self.role,
            sequence: self.sequence,
            relative_path: self.relative_path,
            codec: "pcm_s16le".to_owned(),
            sample_rate: SAMPLE_RATE,
            channels: u32::from(CHANNELS),
            frames_written: self.frames_written,
            clock_start_ns: self.start_ns,
            clock_end_ns,
            sha256,
        })
    }
}

fn write_zeros(file: &mut File, mut count: usize) -> Result<(), std::io::Error> {
    const ZEROS: [u8; 16 * 1024] = [0; 16 * 1024];
    while count > 0 {
        let chunk = count.min(ZEROS.len());
        file.write_all(&ZEROS[..chunk])?;
        count -= chunk;
    }
    Ok(())
}

fn write_wav_header(file: &mut File, frames: u64) -> Result<(), CaptureError> {
    let data_bytes = frames.saturating_mul(u64::from(BLOCK_ALIGN));
    let data_bytes = u32::try_from(data_bytes).map_err(|_| {
        CaptureError::local(
            "invalid_segment",
            "Windows WAV segment exceeds the RIFF size limit",
        )
    })?;
    let riff_bytes = data_bytes.checked_add(36).ok_or_else(|| {
        CaptureError::local(
            "invalid_segment",
            "Windows WAV segment exceeds the RIFF size limit",
        )
    })?;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&riff_bytes.to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&CHANNELS.to_le_bytes());
    header.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    header.extend_from_slice(&(SAMPLE_RATE * u32::from(BLOCK_ALIGN)).to_le_bytes());
    header.extend_from_slice(&BLOCK_ALIGN.to_le_bytes());
    header.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_bytes.to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, CaptureError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn pcm_format() -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CHANNELS,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * u32::from(BLOCK_ALIGN),
        nBlockAlign: BLOCK_ALIGN,
        wBitsPerSample: BITS_PER_SAMPLE,
        cbSize: 0,
    }
}

fn activate_endpoint(id: &str) -> Result<IAudioClient, CaptureError> {
    let enumerator = endpoint_enumerator()?;
    let wide = wide_string(id);
    let device =
        unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }.map_err(map_windows_error)?;
    unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None) }.map_err(map_wasapi_error)
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationCompletion {
    state: Arc<ActivationState>,
}

struct ActivationState {
    interface: AtomicPtr<std::ffi::c_void>,
    complete: Mutex<bool>,
    condition: Condvar,
}

impl Drop for ActivationState {
    fn drop(&mut self) {
        let interface = self.interface.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !interface.is_null() {
            // SAFETY: the callback transferred one owned IAudioClient reference
            // with `into_raw`; reconstructing it here releases that reference.
            drop(unsafe { IAudioClient::from_raw(interface) });
        }
    }
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationCompletion_Impl {
    fn ActivateCompleted(
        &self,
        operation: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let operation = operation.ok()?;
        let mut activation_result = windows::core::HRESULT(0);
        let mut unknown = None;
        let result = unsafe { operation.GetActivateResult(&mut activation_result, &mut unknown) }
            .and_then(|_| activation_result.ok())
            .and_then(|_| {
                unknown
                    .ok_or_else(|| {
                        windows::core::Error::from_hresult(windows::core::HRESULT(
                            0x8000_4003u32 as i32,
                        ))
                    })?
                    .cast::<IAudioClient>()
            })
            .map(Interface::into_raw);
        if let Ok(interface) = result {
            // The activation callback and caller both run in COM's MTA. Keep
            // the owned interface reference alive across the condition signal;
            // the caller reconstructs it after observing completion.
            self.state.interface.store(interface, Ordering::Release);
        }
        if let Ok(mut complete) = self.state.complete.lock() {
            *complete = true;
            self.state.condition.notify_one();
        }
        Ok(())
    }
}

fn activate_process_loopback(pid: u32) -> Result<IAudioClient, CaptureError> {
    let parameters = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let mut variant = unsafe {
        InitPropVariantFromBuffer(
            (&parameters as *const AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
            size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        )
    }
    .map_err(map_windows_error)?;
    let state = Arc::new(ActivationState {
        interface: AtomicPtr::new(std::ptr::null_mut()),
        complete: Mutex::new(false),
        condition: Condvar::new(),
    });
    let callback: IActivateAudioInterfaceCompletionHandler = ActivationCompletion {
        state: Arc::clone(&state),
    }
    .into();
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&variant),
            &callback,
        )
    };
    unsafe {
        let _ = PropVariantClear(&mut variant);
    }
    let _operation = operation.map_err(|_| {
        CaptureError::local(
            "process_loopback_unavailable",
            "Windows process-loopback activation is unavailable on this system",
        )
    })?;
    let guard = state.complete.lock().map_err(|_| worker_ended())?;
    let (_guard, timeout) = state
        .condition
        .wait_timeout_while(guard, std::time::Duration::from_secs(10), |complete| {
            !*complete
        })
        .map_err(|_| worker_ended())?;
    if timeout.timed_out() {
        return Err(CaptureError::local(
            "process_loopback_unavailable",
            "Windows process-loopback activation timed out",
        ));
    }
    let interface = state.interface.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if interface.is_null() {
        return Err(CaptureError::local(
            "process_loopback_unavailable",
            "Windows process-loopback activation failed",
        ));
    }
    Ok(unsafe { IAudioClient::from_raw(interface) })
}

fn endpoint_enumerator() -> Result<IMMDeviceEnumerator, CaptureError> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(map_windows_error)
}

fn enumerate_endpoints(
    enumerator: &IMMDeviceEnumerator,
    flow: windows::Win32::Media::Audio::EDataFlow,
    is_microphone: bool,
) -> Result<Vec<WindowsCaptureSource>, CaptureError> {
    let collection = unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }
        .map_err(map_windows_error)?;
    let count = unsafe { collection.GetCount() }.map_err(map_windows_error)?;
    let mut sources = Vec::with_capacity(count as usize);
    for index in 0..count {
        let device = unsafe { collection.Item(index) }.map_err(map_windows_error)?;
        let id = endpoint_id(&device)?;
        let label = endpoint_label(&device).unwrap_or_else(|| id.clone());
        sources.push(WindowsCaptureSource::Endpoint {
            label,
            id,
            is_microphone,
        });
    }
    Ok(sources)
}

fn endpoint_label(device: &IMMDevice) -> Option<String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let mut value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.ok()?;
    let label = unsafe { PropVariantToStringAlloc(&value) }.ok();
    unsafe {
        let _ = PropVariantClear(&mut value);
    }
    let label = label?;
    let text = unsafe { label.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(label.0.cast())) };
    text.filter(|value| !value.is_empty())
}

fn endpoint_id(device: &IMMDevice) -> Result<String, CaptureError> {
    let id = unsafe { device.GetId() }.map_err(map_windows_error)?;
    let text = unsafe { id.to_string() }.map_err(|_| {
        CaptureError::local(
            "windows_capture_failed",
            "Windows audio endpoint ID is invalid UTF-16",
        )
    });
    unsafe { CoTaskMemFree(Some(id.0.cast())) };
    text
}

fn enumerate_processes() -> Result<Vec<WindowsCaptureSource>, CaptureError> {
    with_process_snapshot(|entry| {
        let label = utf16_array_to_string(&entry.szExeFile);
        if label.is_empty() {
            return None;
        }
        let browser = is_browser_executable(&label);
        Some(WindowsCaptureSource::Process {
            id: format!("pid:{}", entry.th32ProcessID),
            pid: entry.th32ProcessID,
            label,
            browser,
        })
    })
}

fn process_exists(pid: u32) -> Result<bool, CaptureError> {
    Ok(
        with_process_snapshot(|entry| (entry.th32ProcessID == pid).then_some(()))?
            .into_iter()
            .next()
            .is_some(),
    )
}

fn with_process_snapshot<T>(
    mut map: impl FnMut(&PROCESSENTRY32W) -> Option<T>,
) -> Result<Vec<T>, CaptureError> {
    let snapshot =
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(map_windows_error)?;
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut values = Vec::new();
    let mut next = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while next {
        if let Some(value) = map(&entry) {
            values.push(value);
        }
        next = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    unsafe {
        let _ = CloseHandle(snapshot);
    }
    Ok(values)
}

fn selected_process_id(value: &str) -> Result<u32, CaptureError> {
    value
        .strip_prefix("pid:")
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(|| {
            CaptureError::local(
                "invalid_windows_process",
                "Windows meeting source must be an explicitly enumerated process ID",
            )
        })
}

fn is_browser_executable(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "chrome.exe" | "msedge.exe" | "firefox.exe" | "brave.exe" | "opera.exe"
    )
}

fn utf16_array_to_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

fn wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn current_qpc_100ns() -> Result<u64, CaptureError> {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let mut counter = 0i64;
    let mut frequency = 0i64;
    unsafe {
        QueryPerformanceCounter(&mut counter).map_err(map_windows_error)?;
        QueryPerformanceFrequency(&mut frequency).map_err(map_windows_error)?;
    }
    if counter < 0 || frequency <= 0 {
        return Err(CaptureError::local(
            "windows_clock_unavailable",
            "Windows performance counter is unavailable",
        ));
    }
    Ok((counter as u64).saturating_mul(10_000_000) / frequency as u64)
}

fn windows_build_number() -> Result<u32, CaptureError> {
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    let status = unsafe { RtlGetVersion(&mut version) };
    if status.0 < 0 {
        return Err(CaptureError::local(
            "windows_version_unavailable",
            "Windows build number could not be determined",
        ));
    }
    Ok(version.dwBuildNumber)
}

fn role_name(role: TrackRole) -> &'static str {
    match role {
        TrackRole::Microphone => "microphone",
        TrackRole::System => "system",
        TrackRole::Mixed => "mixed",
        TrackRole::Imported => "imported",
    }
}

fn unavailable_preflight(message: &str) -> CapturePreflight {
    CapturePreflight {
        backend_available: false,
        microphone_permission: PermissionState::Unavailable,
        system_audio_permission: PermissionState::Unavailable,
        selected_source: SourceAvailability::Unavailable,
        warnings: vec![message.to_owned()],
    }
}

fn map_wasapi_error(error: windows::core::Error) -> CaptureError {
    if error.code() == windows::core::HRESULT(0x8007_0005u32 as i32) {
        CaptureError::local(
            "microphone_privacy_denied",
            "Windows microphone privacy settings denied access",
        )
    } else if error.code() == AUDCLNT_E_DEVICE_INVALIDATED {
        CaptureError::local(
            "wasapi_device_invalidated",
            "Windows audio device was invalidated",
        )
    } else {
        CaptureError::local("windows_capture_failed", "Windows audio capture failed")
    }
}

fn map_windows_error(_: windows::core::Error) -> CaptureError {
    CaptureError::local(
        "windows_capture_failed",
        "Windows native capture operation failed",
    )
}

struct ComApartment(bool);

impl ComApartment {
    fn initialize() -> Result<Self, CaptureError> {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            return Ok(Self(true));
        }
        if result.0 == 0x8001_0106u32 as i32 {
            return Ok(Self(false));
        }
        Err(map_windows_error(result.into()))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod abi_tests {
    use super::*;

    #[test]
    fn process_loopback_abi_and_flags_match_windows_contract() {
        assert_eq!(AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK.0, 1);
        assert_eq!(PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE.0, 0);
        assert_eq!(AUDCLNT_STREAMFLAGS_LOOPBACK, 0x0002_0000);
        assert_eq!(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 0x0004_0000);
        assert_eq!(AUDCLNT_BUFFERFLAGS_SILENT.0, 2);
        assert!(size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() >= 12);
    }
}
