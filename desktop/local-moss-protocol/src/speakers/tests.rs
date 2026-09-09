use super::*;

fn segment(start_ms: u64, end_ms: u64, speaker: Option<&str>) -> TimingSegment {
    TimingSegment {
        start_ms,
        end_ms,
        speaker: speaker.map(str::to_owned),
    }
}

fn one_window(end_ms: u64) -> Vec<Window> {
    vec![Window {
        index: 0,
        start_ms: 0,
        end_ms,
    }]
}

fn map(moss: &[TimingSegment], anchors: &[TimingSegment]) -> Result<MappingResult, MappingError> {
    reconcile(moss, anchors, &one_window(10_000), 10_000)
}

#[test]
fn nonoverlapping_duplicates_share_anchors_but_actual_overlap_cannot() {
    let anchors = [segment(0, 4000, Some("a")), segment(1000, 4000, Some("b"))];
    let result = map(
        &[
            segment(0, 1000, Some("m1")),
            segment(1000, 2000, Some("m2")),
        ],
        &anchors,
    )
    .unwrap();
    assert_eq!(result.assignments, vec![Some("a".into()), Some("a".into())]);
    assert!(!result.windows[0].conflict_edges[0][1]);
    let result = map(
        &[
            segment(0, 3000, Some("m1")),
            segment(1000, 4000, Some("m2")),
        ],
        &anchors,
    )
    .unwrap();
    assert_eq!(result.assignments, vec![Some("a".into()), Some("b".into())]);
    assert!(result.windows[0].conflict_edges[0][1]);
}

#[test]
fn existing_unknowns_are_fixed_and_zero_support_never_invents_a_slot() {
    let moss = [segment(0, 1000, None), segment(500, 1500, Some("m1"))];
    let result = map(&moss, &[segment(0, 2000, Some("a"))]).unwrap();
    assert_eq!(result.assignments, vec![None, Some("a".into())]);
    assert!(!result.windows[0].unknown_allowed[0]);
    assert_eq!(
        map(&moss, &[segment(5000, 6000, Some("a"))]),
        Err(MappingError::Infeasible)
    );
    let unsupported = map(
        &[segment(0, 1000, Some("m1"))],
        &[segment(5000, 6000, Some("a"))],
    )
    .unwrap();
    assert_eq!(unsupported.assignments, vec![None]);
    assert_eq!(unsupported.new_unknown_segments, 1);
    assert_eq!(
        map(
            &[
                segment(0, 2000, Some("m1")),
                segment(1000, 3000, Some("m2"))
            ],
            &[segment(5000, 6000, None)]
        ),
        Err(MappingError::Infeasible)
    );
}

#[test]
fn byte_lexical_order_and_per_window_slot_reuse_match_javascript() {
    let moss = [segment(0, 1000, Some("m2")), segment(0, 1000, Some("m10"))];
    let result = map(
        &moss,
        &[segment(0, 1000, Some("g2")), segment(0, 1000, Some("g10"))],
    )
    .unwrap();
    assert_eq!(result.windows[0].local_slot_order, vec!["m10", "m2"]);
    assert_eq!(
        result.assignments,
        vec![Some("g2".into()), Some("g10".into())]
    );
    let moss = [
        segment(0, 1000, Some("same")),
        segment(5000, 6000, Some("same")),
    ];
    let windows = [
        Window {
            index: 0,
            start_ms: 0,
            end_ms: 5000,
        },
        Window {
            index: 1,
            start_ms: 5000,
            end_ms: 10_000,
        },
    ];
    let result = reconcile(
        &moss,
        &[segment(0, 1000, Some("a")), segment(5000, 6000, Some("b"))],
        &windows,
        10_000,
    )
    .unwrap();
    assert_eq!(result.assignments, vec![Some("a".into()), Some("b".into())]);
}

#[test]
fn indexed_support_handles_gaps_and_cross_speaker_overlap_exactly() {
    let result = map(
        &[segment(200, 800, Some("m"))],
        &[
            segment(0, 300, Some("a")),
            segment(0, 1000, Some("b")),
            segment(500, 1000, Some("a")),
        ],
    )
    .unwrap();
    assert_eq!(
        result.windows[0].support[0].support_by_global,
        BTreeMap::from([("a".into(), 400), ("b".into(), 600)])
    );
    assert_eq!(result.windows[0].support[0].total_overlap_ms, 1000);
    assert_eq!(result.windows[0].support[0].local_duration_ms, 600);
}

#[test]
fn pcm_window_end_is_explicit_and_never_filled_to_container_end() {
    let windows = one_window(5000);
    let anchors = [segment(0, 5064, Some("a"))];
    let moss = [segment(4000, 5000, Some("m"))];
    let before = moss.clone();
    assert!(reconcile(&moss, &anchors, &windows, 5064).is_ok());
    assert_eq!(moss, before);
    assert_eq!(
        reconcile(&[segment(4990, 5060, Some("m"))], &anchors, &windows, 5064),
        Err(MappingError::SegmentOutsideWindow)
    );
    assert_eq!(
        reconcile(&[segment(5000, 5064, Some("m"))], &anchors, &windows, 5064),
        Err(MappingError::SegmentOutsideWindow)
    );
}

#[test]
fn malformed_or_oversized_inputs_fail_closed() {
    let moss = [segment(0, 1000, Some("m"))];
    let anchors = [segment(0, 1000, Some("a"))];
    assert_eq!(
        reconcile(&moss, &anchors, &one_window(1000), 0),
        Err(MappingError::InvalidDuration)
    );
    assert_eq!(
        reconcile(&moss, &anchors, &one_window(1000), u64::MAX),
        Err(MappingError::InvalidDuration)
    );
    for bad in [
        "",
        "local_unknown",
        "not-an-id",
        "世界",
        "a b",
        "a\0b",
        &"x".repeat(MAX_ID_BYTES + 1),
    ] {
        assert_eq!(
            map(&[segment(0, 1000, Some(bad))], &anchors),
            Err(MappingError::InvalidId)
        );
    }
    assert_eq!(map(&[], &anchors), Err(MappingError::SegmentLimit));
    assert_eq!(
        map(
            &vec![segment(0, 1, Some("m")); MAX_TIMING_SEGMENTS + 1],
            &anchors
        ),
        Err(MappingError::SegmentLimit)
    );
    assert_eq!(
        map(&[segment(5, 5, Some("m"))], &anchors),
        Err(MappingError::InvalidTiming)
    );
    assert_eq!(
        map(&[segment(0, u64::MAX, Some("m"))], &anchors),
        Err(MappingError::InvalidTiming)
    );
    assert_eq!(
        map(
            &[segment(100, 200, Some("m")), segment(50, 150, Some("n"))],
            &anchors
        ),
        Err(MappingError::InvalidTiming)
    );
    assert_eq!(
        map(
            &[segment(0, 2000, Some("m")), segment(1000, 3000, Some("m"))],
            &anchors
        ),
        Err(MappingError::SameSpeakerOverlap)
    );
    assert_eq!(
        map(&moss, &[segment(0, 1000, None), segment(500, 1500, None)]),
        Err(MappingError::SameSpeakerOverlap)
    );
    assert_eq!(
        reconcile(&moss, &anchors, &[], 10_000),
        Err(MappingError::InvalidWindows)
    );
    for windows in [
        vec![Window {
            index: 1,
            start_ms: 0,
            end_ms: 1000,
        }],
        vec![Window {
            index: 0,
            start_ms: 1,
            end_ms: 1000,
        }],
        vec![Window {
            index: 0,
            start_ms: 0,
            end_ms: 10_001,
        }],
        vec![
            Window {
                index: 0,
                start_ms: 0,
                end_ms: 1000
            };
            MAX_WINDOWS + 1
        ],
    ] {
        assert_eq!(
            reconcile(&moss, &anchors, &windows, 10_000),
            Err(MappingError::InvalidWindows)
        );
    }
    let many: Vec<_> = (0..17)
        .map(|i| segment(i * 10, i * 10 + 5, Some(&format!("s{i}"))))
        .collect();
    assert_eq!(map(&many, &anchors), Err(MappingError::SlotLimit));
    assert_eq!(map(&moss, &many), Err(MappingError::SlotLimit));
}

#[test]
fn cancellation_and_resource_limits_never_return_partial_results() {
    let moss = [segment(0, 1000, Some("m"))];
    let anchors = [segment(0, 1000, Some("a"))];
    assert_eq!(
        reconcile_with_cancel(&moss, &anchors, &one_window(1000), 1000, || true),
        Err(MappingError::Cancelled)
    );
    let many: Vec<_> = (0..2000)
        .map(|i| segment(i * 2, i * 2 + 1, Some("m")))
        .collect();
    let mut polls = 0;
    assert_eq!(
        reconcile_with_cancel(
            &many,
            &[segment(0, 4000, Some("a"))],
            &one_window(4000),
            4000,
            || {
                polls += 1;
                polls == 3
            }
        ),
        Err(MappingError::Cancelled)
    );
    assert_eq!(polls, 3);
    let mut cancelled = || false;
    let mut work = Work::new(&mut cancelled).unwrap();
    assert!(matches!(
        solver::solve_with_limit(&[vec![1]], &[vec![false]], &[true], 1, &mut work),
        Err(MappingError::SearchLimit)
    ));
    work.remaining = 0;
    assert_eq!(work.tick(), Err(MappingError::WorkLimit));
    let mut work = Work::new(&mut cancelled).unwrap();
    assert!(matches!(
        solver::solve(
            &[vec![u64::MAX], vec![1]],
            &[vec![false; 2], vec![false; 2]],
            &[true; 2],
            &mut work
        ),
        Err(MappingError::ArithmeticOverflow)
    ));
}

#[test]
fn optimality_and_ties_match_exhaustive_small_graphs() {
    fn brute(
        weights: &[Vec<u64>],
        edges: &[Vec<bool>],
        unknown: &[bool],
    ) -> Option<(u64, Vec<Option<usize>>)> {
        fn visit(
            weights: &[Vec<u64>],
            edges: &[Vec<bool>],
            unknown: &[bool],
            colors: &mut Vec<usize>,
            score: u64,
            best: &mut Option<(u64, Vec<Option<usize>>)>,
        ) {
            let row = colors.len();
            if row == weights.len() {
                if best.as_ref().is_none_or(|(previous, _)| score > *previous) {
                    *best = Some((
                        score,
                        colors
                            .iter()
                            .map(|color| (*color != 2).then_some(*color))
                            .collect(),
                    ));
                }
                return;
            }
            for color in 0..3 {
                if (color == 2 && !unknown[row])
                    || (color < 2 && weights[row][color] == 0)
                    || colors
                        .iter()
                        .enumerate()
                        .any(|(previous, assigned)| edges[row][previous] && *assigned == color)
                {
                    continue;
                }
                colors.push(color);
                visit(
                    weights,
                    edges,
                    unknown,
                    colors,
                    score + if color == 2 { 0 } else { weights[row][color] },
                    best,
                );
                colors.pop();
            }
        }
        let mut best = None;
        visit(weights, edges, unknown, &mut Vec::new(), 0, &mut best);
        best
    }
    for graph in 0..8 {
        let mut edges = vec![vec![false; 3]; 3];
        for (bit, (i, j)) in [(0, 1), (0, 2), (1, 2)].into_iter().enumerate() {
            edges[i][j] = graph & (1 << bit) != 0;
            edges[j][i] = edges[i][j];
        }
        for bits in 0..64 {
            let weights: Vec<Vec<u64>> = (0..3)
                .map(|row| {
                    (0..2)
                        .map(|column| (bits >> (row * 2 + column)) & 1)
                        .collect()
                })
                .collect();
            for mask in 0..8 {
                let unknown: Vec<bool> = (0..3).map(|row| mask & (1 << row) != 0).collect();
                let mut cancelled = || false;
                let mut work = Work::new(&mut cancelled).unwrap();
                match (
                    brute(&weights, &edges, &unknown),
                    solver::solve(&weights, &edges, &unknown, &mut work),
                ) {
                    (None, Err(MappingError::Infeasible)) => {}
                    (Some((score, columns)), Ok(result)) => {
                        assert_eq!(result.total_support_ms, score);
                        assert_eq!(result.columns, columns);
                    }
                    _ => panic!("exhaustive_solver_mismatch"),
                }
            }
        }
    }
    let mut cancelled = || false;
    let mut work = Work::new(&mut cancelled).unwrap();
    let result = solver::solve(
        &[vec![10, 9], vec![9, 0]],
        &[vec![false, true], vec![true, false]],
        &[true; 2],
        &mut work,
    )
    .unwrap();
    assert_eq!(result.total_support_ms, 18);
    assert_eq!(result.columns, vec![Some(1), Some(0)]);
}

#[test]
fn long_timing_inputs_use_indexed_support_with_bounded_work() {
    let moss: Vec<_> = (0..20_000)
        .map(|i| segment(i * 2, i * 2 + 1, Some("m")))
        .collect();
    let anchors: Vec<_> = (0..20_000)
        .map(|i| segment(i * 2, i * 2 + 1, Some("a")))
        .collect();
    let result = reconcile(&moss, &anchors, &one_window(40_000), 40_000).unwrap();
    assert_eq!(result.assignments.len(), 20_000);
    assert_eq!(result.windows[0].total_support_ms, 20_000);
    assert!(result.work_units < 1_000_000);
}
