use super::super::session::CaptureError;

pub const QPC_100NS_TO_NS: u64 = 100;
const DEFAULT_DRIFT_LIMIT_NS: u64 = 100_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PacketFlags(u32);

impl PacketFlags {
    pub const DATA_DISCONTINUITY: Self = Self(1);
    pub const SILENT: Self = Self(2);
    pub const TIMESTAMP_ERROR: Self = Self(4);

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasapiPacket {
    pub frames: u32,
    pub device_position: u64,
    /// WASAPI reports the packet QPC position in 100-nanosecond units.
    pub qpc_100ns: u64,
    pub flags: PacketFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockMapper {
    qpc_origin_100ns: u64,
    monotonic_origin_ns: u64,
}

impl ClockMapper {
    pub const fn new(qpc_origin_100ns: u64, monotonic_origin_ns: u64) -> Self {
        Self {
            qpc_origin_100ns,
            monotonic_origin_ns,
        }
    }

    pub fn map(self, qpc_100ns: u64) -> Result<u64, CaptureError> {
        let delta = qpc_100ns
            .checked_sub(self.qpc_origin_100ns)
            .ok_or_else(|| {
                CaptureError::local(
                    "invalid_wasapi_timestamp",
                    "WASAPI packet timestamp precedes the capture clock origin",
                )
            })?;
        self.monotonic_origin_ns
            .checked_add(delta.saturating_mul(QPC_100NS_TO_NS))
            .ok_or_else(|| {
                CaptureError::local(
                    "invalid_wasapi_timestamp",
                    "WASAPI packet timestamp exceeds the capture clock range",
                )
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketIssue {
    DataDiscontinuity {
        expected_device_position: u64,
        actual_device_position: u64,
    },
    TimestampError,
    ClockDrift {
        expected_clock_ns: u64,
        actual_clock_ns: u64,
        drift_ns: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketAnalysis {
    pub clock_start_ns: u64,
    pub clock_end_ns: u64,
    pub silent: bool,
    pub issues: Vec<PacketIssue>,
}

#[derive(Debug, Clone)]
pub struct PacketTimeline {
    mapper: ClockMapper,
    sample_rate: u32,
    expected_device_position: Option<u64>,
    previous_clock_end_ns: Option<u64>,
    drift_limit_ns: u64,
}

impl PacketTimeline {
    pub fn new(mapper: ClockMapper, sample_rate: u32) -> Result<Self, CaptureError> {
        if sample_rate == 0 || sample_rate > 768_000 {
            return Err(CaptureError::local(
                "invalid_audio_format",
                "WASAPI sample rate is invalid",
            ));
        }
        Ok(Self {
            mapper,
            sample_rate,
            expected_device_position: None,
            previous_clock_end_ns: None,
            drift_limit_ns: DEFAULT_DRIFT_LIMIT_NS,
        })
    }

    #[cfg(test)]
    pub fn with_drift_limit_ns(mut self, limit: u64) -> Self {
        self.drift_limit_ns = limit;
        self
    }

    pub fn observe(&mut self, packet: &WasapiPacket) -> Result<PacketAnalysis, CaptureError> {
        if packet.frames == 0 {
            return Err(CaptureError::local(
                "invalid_wasapi_packet",
                "WASAPI packet contains no frames",
            ));
        }
        let clock_start_ns = self.mapper.map(packet.qpc_100ns)?;
        let duration_ns =
            u64::from(packet.frames).saturating_mul(1_000_000_000) / u64::from(self.sample_rate);
        let clock_end_ns = clock_start_ns.saturating_add(duration_ns);
        let mut issues = Vec::new();

        if packet.flags.contains(PacketFlags::TIMESTAMP_ERROR) {
            issues.push(PacketIssue::TimestampError);
        }
        if let Some(expected) = self.expected_device_position {
            if packet.flags.contains(PacketFlags::DATA_DISCONTINUITY)
                || packet.device_position != expected
            {
                issues.push(PacketIssue::DataDiscontinuity {
                    expected_device_position: expected,
                    actual_device_position: packet.device_position,
                });
            }
        }
        if let Some(expected_clock_ns) = self.previous_clock_end_ns {
            let drift_ns = expected_clock_ns.abs_diff(clock_start_ns);
            if drift_ns > self.drift_limit_ns {
                issues.push(PacketIssue::ClockDrift {
                    expected_clock_ns,
                    actual_clock_ns: clock_start_ns,
                    drift_ns,
                });
            }
        }

        self.expected_device_position = Some(
            packet
                .device_position
                .saturating_add(u64::from(packet.frames)),
        );
        self.previous_clock_end_ns = Some(clock_end_ns);
        Ok(PacketAnalysis {
            clock_start_ns,
            clock_end_ns,
            silent: packet.flags.contains(PacketFlags::SILENT),
            issues,
        })
    }
}
