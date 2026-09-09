use super::*;

#[test]
fn qpc_positions_align_independent_mic_and_system_streams() {
    let mapper = ClockMapper::new(10_000, 2_000_000_000);
    let mut mic = PacketTimeline::new(mapper, 48_000).unwrap();
    let mut system = PacketTimeline::new(mapper, 48_000).unwrap();
    let mic_packet = WasapiPacket {
        frames: 480,
        device_position: 4_800,
        qpc_100ns: 20_000,
        flags: PacketFlags::default(),
    };
    let system_packet = WasapiPacket {
        frames: 480,
        device_position: 9_600,
        qpc_100ns: 20_000,
        flags: PacketFlags::default(),
    };

    let mic_result = mic.observe(&mic_packet).unwrap();
    let system_result = system.observe(&system_packet).unwrap();
    assert_eq!(mic_result.clock_start_ns, 2_001_000_000);
    assert_eq!(mic_result.clock_start_ns, system_result.clock_start_ns);
    assert_eq!(mic_result.clock_end_ns, system_result.clock_end_ns);
}

#[test]
fn discontinuity_and_device_position_gap_are_explicit() {
    let mapper = ClockMapper::new(0, 0);
    let mut timeline = PacketTimeline::new(mapper, 48_000).unwrap();
    timeline
        .observe(&WasapiPacket {
            frames: 480,
            device_position: 0,
            qpc_100ns: 0,
            flags: PacketFlags::default(),
        })
        .unwrap();
    let result = timeline
        .observe(&WasapiPacket {
            frames: 480,
            device_position: 960,
            qpc_100ns: 200_000,
            flags: PacketFlags::DATA_DISCONTINUITY,
        })
        .unwrap();

    assert!(result.issues.iter().any(|issue| matches!(
        issue,
        PacketIssue::DataDiscontinuity {
            expected_device_position: 480,
            actual_device_position: 960
        }
    )));
}

#[test]
fn timestamp_error_and_clock_drift_are_not_hidden() {
    let mapper = ClockMapper::new(0, 0);
    let mut timeline = PacketTimeline::new(mapper, 48_000)
        .unwrap()
        .with_drift_limit_ns(1_000_000);
    timeline
        .observe(&WasapiPacket {
            frames: 480,
            device_position: 0,
            qpc_100ns: 0,
            flags: PacketFlags::default(),
        })
        .unwrap();
    let result = timeline
        .observe(&WasapiPacket {
            frames: 480,
            device_position: 480,
            qpc_100ns: 500_000,
            flags: PacketFlags::TIMESTAMP_ERROR,
        })
        .unwrap();

    assert!(result.issues.contains(&PacketIssue::TimestampError));
    assert!(result
        .issues
        .iter()
        .any(|issue| matches!(issue, PacketIssue::ClockDrift { .. })));
}

#[test]
fn windows_contract_constants_do_not_regress() {
    assert_eq!(PROCESS_LOOPBACK_MINIMUM_BUILD, 20_348);
    assert_eq!(
        MICROPHONE_PRIVACY_SETTINGS_URI,
        "ms-settings:privacy-microphone"
    );
    assert!(!std::hint::black_box(PROTECTED_AUDIO_SUPPORTED));
    assert_eq!(PacketFlags::DATA_DISCONTINUITY.bits(), 1);
    assert_eq!(PacketFlags::SILENT.bits(), 2);
    assert_eq!(PacketFlags::TIMESTAMP_ERROR.bits(), 4);
}
