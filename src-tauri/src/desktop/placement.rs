//! Window placement in a monitor's work-area coordinate space, not the current screen's.
//! On macOS Tao reports global points multiplied by the reporting monitor/window scale;
//! those rectangles need not form a single, unambiguous mixed-DPI pixel plane.
use crate::preferences::WindowOrigin;

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkArea {
    pub origin: (i32, i32),
    pub size: (u32, u32),
    pub scale: f64,
}
impl From<&tauri::Monitor> for WorkArea {
    fn from(monitor: &tauri::Monitor) -> Self {
        let area = monitor.work_area();
        Self {
            origin: (area.position.x, area.position.y),
            size: (area.size.width, area.size.height),
            scale: monitor.scale_factor(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub origin: WindowOrigin,
    pub scale: f64,
}
impl Placement {
    pub fn position(self) -> tauri::Position {
        if cfg!(target_os = "macos") {
            // Tao 0.35.3 set_outer_position converts Physical with the CURRENT window's
            // scale. Explicit points avoid corrupting a move to a differently scaled screen.
            tauri::LogicalPosition::new(
                f64::from(self.origin.x) / self.scale,
                f64::from(self.origin.y) / self.scale,
            )
            .into()
        } else {
            tauri::PhysicalPosition::new(self.origin.x, self.origin.y).into()
        }
    }
}

impl WorkArea {
    fn window_pixels(self, logical_size: (f64, f64)) -> Option<(f64, f64)> {
        if self.size.0 == 0
            || self.size.1 == 0
            || !self.scale.is_finite()
            || !(0.25..=8.0).contains(&self.scale)
            || ![logical_size.0, logical_size.1]
                .iter()
                .all(|n| n.is_finite() && *n > 0.0)
        {
            return None;
        }
        let pixels = (logical_size.0 * self.scale, logical_size.1 * self.scale);
        [pixels.0, pixels.1]
            .iter()
            .all(|n| n.is_finite() && *n <= f64::from(u32::MAX))
            .then_some(pixels)
    }
    fn clamp(self, x: f64, y: f64, size: (f64, f64)) -> Option<Placement> {
        let left = f64::from(self.origin.0);
        let top = f64::from(self.origin.1);
        let x = x
            .clamp(left, (left + f64::from(self.size.0) - size.0).max(left))
            .round();
        let y = y
            .clamp(top, (top + f64::from(self.size.1) - size.1).max(top))
            .round();
        if ![x, y]
            .iter()
            .all(|n| n.is_finite() && *n >= f64::from(i32::MIN) && *n <= f64::from(i32::MAX))
        {
            return None;
        }
        Some(Placement {
            origin: WindowOrigin {
                x: x as i32,
                y: y as i32,
            },
            scale: self.scale,
        })
    }
}

/// Restore only when exactly one available monitor contains >=60% of each window axis.
/// Never clamp an unplugged/ambiguous secondary-screen origin onto the current screen.
pub(crate) fn restore(
    saved: WindowOrigin,
    monitors: &[WorkArea],
    logical_size: (f64, f64),
) -> Option<Placement> {
    let mut candidates = monitors.iter().filter_map(|area| {
        let size = area.window_pixels(logical_size)?;
        let (x, y) = (f64::from(saved.x), f64::from(saved.y));
        let (left, top) = (f64::from(area.origin.0), f64::from(area.origin.1));
        let visible_width = (x + size.0).min(left + f64::from(area.size.0)) - x.max(left);
        let visible_height = (y + size.1).min(top + f64::from(area.size.1)) - y.max(top);
        if visible_width < size.0 * 0.6 || visible_height < size.1 * 0.6 {
            return None;
        }
        area.clamp(x, y, size)
    });
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

pub(crate) fn centered(area: WorkArea, logical_size: (f64, f64)) -> Option<Placement> {
    let size = area.window_pixels(logical_size)?;
    area.clamp(
        f64::from(area.origin.0) + (f64::from(area.size.0) - size.0) / 2.0,
        f64::from(area.origin.1) + (f64::from(area.size.1) - size.1) / 2.0,
        size,
    )
}

/// A work-area-safe fallback only; actual tray-rectangle anchoring remains a P0 gate.
pub(crate) fn popover_fallback(area: WorkArea, logical_size: (f64, f64)) -> Option<Placement> {
    let size = area.window_pixels(logical_size)?;
    area.clamp(
        f64::from(area.origin.0) + f64::from(area.size.0) - size.0 - 24.0 * area.scale,
        f64::from(area.origin.1) + f64::from(area.size.1) - size.1 - 12.0 * area.scale,
        size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const LOGICAL: (f64, f64) = (380.0, 580.0);
    const RETINA: WorkArea = WorkArea {
        origin: (0, 48),
        size: (2940, 1740),
        scale: 2.0,
    };
    const EXTERNAL: WorkArea = WorkArea {
        origin: (-1920, -1055),
        size: (1920, 1055),
        scale: 1.0,
    };

    #[test]
    fn saved_secondary_screen_is_selected_instead_of_current_retina_screen() {
        let saved = WindowOrigin { x: -1800, y: -1000 };
        let placement = restore(saved, &[RETINA, EXTERNAL], LOGICAL).unwrap();
        assert_eq!(placement.origin, saved);
        assert_eq!(placement.scale, 1.0);
        let reverse = restore(
            WindowOrigin { x: 200, y: 100 },
            &[EXTERNAL, RETINA],
            LOGICAL,
        )
        .unwrap();
        assert_eq!(reverse.scale, 2.0);
        if cfg!(target_os = "macos") {
            // Same target points regardless of the window's OLD scale factor.
            assert_eq!(
                placement.position().to_logical::<f64>(2.0),
                tauri::LogicalPosition::new(-1800.0, -1000.0)
            );
            assert_eq!(
                reverse.position().to_logical::<f64>(1.0),
                tauri::LogicalPosition::new(100.0, 50.0)
            );
        } else {
            assert_eq!(
                placement.position().to_physical::<i32>(2.0),
                tauri::PhysicalPosition::new(-1800, -1000)
            );
            assert_eq!(
                reverse.position().to_physical::<i32>(1.0),
                tauri::PhysicalPosition::new(200, 100)
            );
        }
    }

    #[test]
    fn retina_restore_uses_actual_scaled_window_size_and_excludes_dock_and_menu_bar() {
        let placed = restore(WindowOrigin { x: 2200, y: 800 }, &[RETINA], LOGICAL).unwrap();
        assert_eq!(placed.origin, WindowOrigin { x: 2180, y: 628 });
        assert!(f64::from(placed.origin.x) + LOGICAL.0 * placed.scale <= 2940.0);
        assert!(f64::from(placed.origin.y) + LOGICAL.1 * placed.scale <= 1788.0);
        let changed_size =
            restore(WindowOrigin { x: 200, y: 40 }, &[RETINA], (500.0, 650.0)).unwrap();
        assert_eq!(changed_size.origin.y, 48);
    }

    #[test]
    fn unplugged_or_ambiguous_monitor_uses_explicit_safe_default_not_guessed_clamp() {
        assert!(restore(WindowOrigin { x: -1800, y: -1000 }, &[RETINA], LOGICAL).is_none());
        assert!(restore(WindowOrigin { x: 200, y: 100 }, &[RETINA, RETINA], LOGICAL).is_none());
        // Per-monitor multiplication makes physical coordinate ranges overlap on macOS.
        let overlap = WorkArea {
            origin: (1200, 25),
            size: (1920, 1055),
            scale: 1.0,
        };
        assert!(restore(
            WindowOrigin { x: 1300, y: 100 },
            &[RETINA, overlap],
            LOGICAL
        )
        .is_none());
        assert_eq!(
            centered(RETINA, LOGICAL).unwrap().origin,
            WindowOrigin { x: 1090, y: 338 }
        );
    }

    #[test]
    fn zero_nonfinite_and_extreme_geometry_never_panics_or_wraps_coordinates() {
        for scale in [0.0, f64::NAN, f64::INFINITY, -1.0] {
            assert!(centered(WorkArea { scale, ..RETINA }, LOGICAL).is_none());
        }
        for size in [(0.0, 1.0), (1.0, f64::NAN), (f64::INFINITY, 580.0)] {
            assert!(restore(WindowOrigin { x: 0, y: 0 }, &[RETINA], size).is_none());
        }
        assert!(centered(
            WorkArea {
                origin: (i32::MAX, i32::MAX),
                size: (u32::MAX, u32::MAX),
                scale: 1.0
            },
            LOGICAL
        )
        .is_none());
        // A smaller work area still leaves the title/drag area reachable.
        assert_eq!(
            centered(
                WorkArea {
                    size: (400, 400),
                    ..RETINA
                },
                LOGICAL
            )
            .unwrap()
            .origin,
            WindowOrigin { x: 0, y: 48 }
        );
    }

    #[test]
    fn popover_fallback_respects_work_area_offsets_and_target_scale() {
        let area = WorkArea {
            origin: (1470, -93),
            size: (1920, 1080),
            scale: 1.0,
        };
        assert_eq!(
            popover_fallback(area, LOGICAL).unwrap().origin,
            WindowOrigin { x: 2986, y: 395 }
        );
        assert_eq!(
            popover_fallback(EXTERNAL, LOGICAL).unwrap().origin,
            WindowOrigin { x: -404, y: -592 }
        );
        assert_eq!(
            popover_fallback(RETINA, LOGICAL).unwrap().origin,
            WindowOrigin { x: 2132, y: 604 }
        );
    }
}
