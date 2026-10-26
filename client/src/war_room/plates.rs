//! The plates the map draws on war territories, published from the war board.

use std::sync::Arc;

use leptos::prelude::*;
use sequoia_browser_map::BrowserMap;
use sequoia_browser_map::overlay::{PlateLabel, WarPlate, WarPlates};
use sequoia_shared::war_board::{FrontStage, Outcome, WarBoard, WarFront};

use super::{WarRoom, ending};
use crate::app::ShowWarMarkers;
use crate::territory::ClientTerritoryMap;

/// One plate per front and per recent outcome, anchored on the territory's top edge; fighting
/// fronts come last so they draw on top.
pub(super) fn war_plates(
    board: &WarBoard,
    recent: &[Outcome],
    now: i64,
    stale: bool,
    territories: &ClientTerritoryMap,
) -> Option<WarPlates> {
    let anchor = |territory: &str| {
        let region = &territories.get(territory)?.territory.location;
        Some([region.midpoint_x() as f32, region.top() as f32])
    };
    let mut plates: Vec<WarPlate> = recent
        .iter()
        .filter_map(|outcome| {
            Some(WarPlate {
                world: anchor(&outcome.territory)?,
                label: PlateLabel::Ending(ending(outcome, territories)),
            })
        })
        .collect();
    plates.extend(board.fronts.iter().rev().filter_map(|front| {
        Some(WarPlate {
            world: anchor(&front.territory)?,
            label: PlateLabel::Front {
                stage: front.stage_kind(),
                clock: front.clock(now),
                health: measured_health(front),
            },
        })
    }));
    (!plates.is_empty()).then_some(WarPlates { plates, stale })
}

fn measured_health(front: &WarFront) -> Option<f32> {
    match front.stage {
        FrontStage::Fighting {
            tower: Some(tower), ..
        } if tower.is_measured() => Some(tower.health),
        _ => None,
    }
}

/// Publishes the plates to the map whenever what they show changes: every second while a
/// clock is running, otherwise only when the board does.
#[component]
pub(super) fn WarPlatesLayer() -> impl IntoView {
    let room = expect_context::<WarRoom>();
    let ShowWarMarkers(show_markers) = expect_context();
    let territories: RwSignal<ClientTerritoryMap> = expect_context();
    let map = expect_context::<BrowserMap>();
    Effect::new(move || {
        let plates = show_markers
            .get()
            .then(|| {
                room.board.with(|board| {
                    let board = board.as_ref()?;
                    room.outcomes.with(|recent| {
                        territories.with(|map| {
                            war_plates(board, recent, room.now.get(), room.stale.get(), map)
                        })
                    })
                })
            })
            .flatten();
        map.war_plates().maybe_update(|current| {
            if current.as_deref() == plates.as_ref() {
                return false;
            }
            *current = plates.map(Arc::new);
            true
        });
    });
    on_cleanup(move || {
        map.war_plates().try_set(None);
    });
}

#[cfg(test)]
mod tests {
    use sequoia_shared::war_board::{Ending, FrontClock, OutcomeKind, StageKind, Tower};

    use super::*;
    use crate::war_room::tests::map;

    fn front(territory: &str, stage: FrontStage) -> WarFront {
        WarFront {
            territory: territory.to_string(),
            difficulty: None,
            stage,
        }
    }

    fn board(fronts: Vec<WarFront>) -> WarBoard {
        WarBoard {
            fronts,
            roster: Vec::new(),
            unavailable: false,
            as_of: 1_000,
        }
    }

    #[test]
    fn plates_sit_on_each_war_territory_top_edge_with_its_clock() {
        let board = board(vec![
            front(
                "Ragni",
                FrontStage::Fighting {
                    started_at: Some(940),
                    tower: Some(Tower {
                        health: 0.5,
                        ehp: Some(10),
                        dps: Some(2),
                    }),
                    expected_win_at: None,
                },
            ),
            front(
                "Detlas",
                FrontStage::Queued {
                    starts_at: Some(1_128),
                },
            ),
        ]);
        let plates = war_plates(&board, &[], 1_000, false, &map()).expect("plates");
        // Drawn last, on top: the fighting front.
        assert_eq!(plates.plates[1].world, [50.0, 0.0]);
        assert_eq!(
            plates.plates[1].label,
            PlateLabel::Front {
                stage: StageKind::Fighting,
                clock: FrontClock::FightingFor(60),
                health: Some(0.5),
            }
        );
        assert_eq!(
            plates.plates[0].label,
            PlateLabel::Front {
                stage: StageKind::Queued,
                clock: FrontClock::StartsIn(128),
                health: None,
            }
        );
    }

    #[test]
    fn an_unmeasured_tower_draws_no_health_bar() {
        let board = board(vec![front(
            "Ragni",
            FrontStage::Fighting {
                started_at: None,
                tower: Some(Tower {
                    health: 0.9,
                    ehp: None,
                    dps: None,
                }),
                expected_win_at: None,
            },
        )]);
        let plates = war_plates(&board, &[], 1_000, true, &map()).expect("plates");
        assert_eq!(
            plates.plates[0].label,
            PlateLabel::Front {
                stage: StageKind::Fighting,
                clock: FrontClock::Fighting,
                health: None,
            }
        );
        assert!(plates.stale);
    }

    #[test]
    fn an_outcome_plate_reads_by_current_ownership() {
        let recent = [Outcome {
            territory: "Detlas".to_string(),
            kind: OutcomeKind::WarEnded,
            at: 990,
        }];
        let plates = war_plates(&board(Vec::new()), &recent, 1_000, false, &map()).expect("plates");
        assert_eq!(plates.plates[0].label, PlateLabel::Ending(Ending::Captured));
    }

    #[test]
    fn no_fronts_and_no_outcomes_draw_no_plates() {
        assert_eq!(
            war_plates(&board(Vec::new()), &[], 1_000, false, &map()),
            None
        );
    }
}
