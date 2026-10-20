//! Best-effort lane avoidance from mpv's current ASS text/style properties.
//! These are semantic hints, not rendered bounding boxes. Never rewrite the
//! primary track's authored positions or styles to make space for a translation.
use libmpv2::Mpv;
use std::collections::HashMap;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Lanes(u8);
impl Lanes {
    fn add_alignment(&mut self, alignment: u8) {
        self.0 |= match alignment {
            7..=9 => 1,
            4..=6 => 2,
            1..=3 => 4,
            _ => 0,
        };
    }
    fn add_y(&mut self, y: f64, height: Option<f64>) {
        if let Some(height) = height.filter(|height| *height > 0.0)
            && y.is_finite()
        {
            self.0 |= if y < height * 0.35 {
                1
            } else if y > height * 0.65 {
                4
            } else {
                2
            };
        }
    }
    fn occupied(self, position: i64) -> bool {
        self.0
            & (if position < 35 {
                1
            } else if position > 65 {
                4
            } else {
                2
            })
            != 0
    }
    fn best(self) -> i64 {
        if self.0 & 1 == 0 {
            0
        } else if self.0 & 4 == 0 {
            100
        } else if self.0 & 2 == 0 {
            50
        } else {
            0
        } // No proven free lane; retain the documented manual escape.
    }
}

fn legacy_alignment(value: u8) -> u8 {
    match value {
        5..=7 => value + 2,
        9..=11 => value - 5,
        1..=3 => value,
        _ => 0,
    }
}

#[derive(Default)]
struct Styles {
    height: Option<f64>,
    alignments: HashMap<String, u8>,
}
impl Styles {
    fn parse(header: &str) -> Self {
        let mut height = None;
        let mut styles = HashMap::new();
        let mut format = Vec::new();
        let mut style_section = false;
        let mut legacy = false;
        for line in header.lines().take(1024) {
            let line = line.trim();
            if line.starts_with('[') {
                style_section = line == "[V4+ Styles]" || line == "[V4 Styles]";
                legacy = line == "[V4 Styles]";
            }
            if let Some(value) = line.strip_prefix("PlayResY:") {
                height = value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|height| height.is_finite() && *height > 0.0);
            }
            if !style_section {
                continue;
            }
            if let Some(value) = line.strip_prefix("Format:") {
                format = value.split(',').map(str::trim).take(32).collect();
            }
            if let Some(value) = line.strip_prefix("Style:") {
                let values = value.split(',').map(str::trim).take(32).collect::<Vec<_>>();
                let name = format
                    .iter()
                    .position(|name| *name == "Name")
                    .and_then(|index| values.get(index));
                let alignment = format
                    .iter()
                    .position(|name| *name == "Alignment")
                    .and_then(|index| values.get(index))
                    .and_then(|value| value.parse::<u8>().ok());
                if let (Some(name), Some(alignment)) = (name, alignment) {
                    styles.insert(
                        (*name).to_owned(),
                        if legacy {
                            legacy_alignment(alignment)
                        } else {
                            alignment
                        },
                    );
                }
            }
        }
        Self {
            height,
            alignments: styles,
        }
    }
}

#[cfg(test)]
fn lanes(header: &str, events: &str, fallback_pos: f64) -> Lanes {
    event_lanes(&Styles::parse(header), events, fallback_pos)
}
fn event_lanes(styles: &Styles, events: &str, fallback_pos: f64) -> Lanes {
    let height = styles.height;
    let mut result = Lanes::default();
    for line in events.lines().take(64) {
        let fields = line
            .strip_prefix("Dialogue:")
            .map(|line| line.trim_start().splitn(10, ',').collect::<Vec<_>>());
        let (style, text) = fields
            .as_ref()
            .filter(|fields| fields.len() == 10)
            .map(|fields| (styles.alignments.get(fields[3]).copied(), fields[9]))
            .unwrap_or((None, line));
        if text.trim().is_empty() {
            continue;
        }
        let mut event = Lanes::default();
        for block in text
            .split('{')
            .skip(1)
            .take(64)
            .filter_map(|block| block.split_once('}').map(|(tags, _)| tags))
        {
            for tag in block.split('\\').skip(1) {
                if let Some(value) = tag
                    .strip_prefix("an")
                    .and_then(|value| value.chars().next())
                    .and_then(|value| value.to_digit(10))
                {
                    event.add_alignment(value as u8);
                } else if let Some(value) = tag
                    .strip_prefix('a')
                    .and_then(|value| {
                        value
                            .split(|character: char| !character.is_ascii_digit())
                            .next()
                    })
                    .and_then(|value| value.parse::<u8>().ok())
                {
                    event.add_alignment(legacy_alignment(value));
                }
                for (prefix, coordinates) in [("pos(", 2), ("move(", 4)] {
                    if let Some(arguments) = tag.strip_prefix(prefix).and_then(|arguments| {
                        arguments.split_once(')').map(|(arguments, _)| arguments)
                    }) {
                        let values = arguments
                            .split(',')
                            .take(coordinates)
                            .map(|value| value.trim().parse::<f64>().ok())
                            .collect::<Option<Vec<_>>>();
                        if let Some(values) = values.filter(|values| values.len() == coordinates) {
                            event.add_y(values[1], height);
                            if coordinates == 4 {
                                event.add_y(values[3], height);
                                if (values[1] - values[3]).abs()
                                    > height.unwrap_or(f64::INFINITY) * 0.3
                                {
                                    event.0 |= 2;
                                }
                            }
                        }
                    }
                }
            }
        }
        if event.0 == 0 {
            if let Some(style) = style {
                event.add_alignment(style);
            } else {
                event.add_y(fallback_pos, Some(100.0));
            }
        }
        result.0 |= event.0;
    }
    result
}

#[derive(Default)]
pub(super) struct Placement {
    mode: i32,
    applied: Option<i64>,
    cue: Option<f64>,
    source: String,
    primary: String,
    styles: Styles,
    events: String,
    fallback: Option<u64>,
    occupied: Lanes,
}
impl Placement {
    pub(super) fn set_mode(&mut self, index: i32) {
        *self = Self {
            mode: index.clamp(0, 2),
            ..Default::default()
        };
    }
    fn occupancy(
        &mut self,
        source: &str,
        primary: &str,
        events: &str,
        fallback: f64,
        header: impl FnOnce() -> String,
    ) -> Lanes {
        let changed_track = self.source != source || self.primary != primary;
        if changed_track {
            self.styles = Styles::parse(&header());
            self.source = source.to_owned();
            self.primary = primary.to_owned();
        }
        if changed_track || self.events != events || self.fallback != Some(fallback.to_bits()) {
            self.occupied = event_lanes(&self.styles, events, fallback);
            self.events = events.to_owned();
            self.fallback = Some(fallback.to_bits());
        }
        self.occupied
    }

    pub(super) fn apply(&mut self, mpv: &Mpv) {
        if mpv
            .get_property::<String>("secondary-sid")
            .ok()
            .and_then(|id| id.parse::<i32>().ok())
            .is_none()
        {
            return;
        }
        let mut desired = match self.mode {
            1 => 0,
            2 => 100,
            _ => 0,
        };
        if self.mode == 0 {
            let events = mpv
                .get_property::<String>("sub-text/ass-full")
                .or_else(|_| mpv.get_property::<String>("sub-text-ass"))
                .unwrap_or_default();
            let source = mpv.get_property::<String>("path").unwrap_or_default();
            let primary = mpv.get_property::<String>("sid").unwrap_or_default();
            let fallback = mpv.get_property::<f64>("sub-pos").unwrap_or(100.0);
            let occupied = self.occupancy(&source, &primary, &events, fallback, || {
                mpv.get_property::<String>("sub-ass-extradata")
                    .unwrap_or_default()
            });
            desired = occupied.best();
            let cue = mpv.get_property::<f64>("secondary-sub-start").ok();
            // Keep a still-free lane through subtitle gaps. Move immediately
            // when primary text starts using it; otherwise wait for a new cue.
            if let Some(previous) = self.applied
                && cue == self.cue
                && !occupied.occupied(previous)
            {
                desired = previous;
            }
            self.cue = cue;
        }
        if mpv.get_property::<f64>("secondary-sub-pos").ok() != Some(desired as f64)
            && mpv.set_property("secondary-sub-pos", desired).is_err()
        {
            return;
        }
        self.applied = Some(desired);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HEADER: &str = "[Script Info]\nPlayResY: 1080\n[V4+ Styles]\nFormat: Name, Alignment\nStyle: Main,2\nStyle: Signs,8\n";
    #[test]
    fn style_cache_is_scoped_to_actual_source_and_primary_track() {
        let reads = std::cell::Cell::new(0);
        let mut placement = Placement::default();
        let header = || {
            reads.set(reads.get() + 1);
            HEADER.to_owned()
        };
        let event = "Dialogue: 0,0:00:01.00,0:00:03.00,Signs,,0,0,0,,A sign";
        assert_eq!(
            placement
                .occupancy("source-a", "1", event, 100.0, header)
                .best(),
            100
        );
        assert_eq!(
            placement
                .occupancy("source-a", "1", event, 100.0, header)
                .best(),
            100
        );
        assert_eq!(reads.get(), 1, "unchanged cues reuse parsed style metadata");
        placement.occupancy("source-a", "2", event, 100.0, header);
        placement.occupancy("source-b", "2", event, 100.0, header);
        assert_eq!(
            reads.get(),
            3,
            "numeric track IDs cannot alias another source's styles"
        );
    }

    #[test]
    fn authored_top_and_bottom_events_leave_main_styles_intact_and_choose_free_lane() {
        let top = "Dialogue: 0,0:00:01.00,0:00:03.00,Signs,,0,0,0,,A sign";
        let bottom = "Dialogue: 0,0:00:01.00,0:00:03.00,Main,,0,0,0,,Dialogue";
        assert_eq!(lanes(HEADER, top, 100.0).best(), 100);
        assert_eq!(lanes(HEADER, bottom, 100.0).best(), 0);
        assert_eq!(lanes(HEADER, &format!("{top}\n{bottom}"), 100.0).best(), 50);
        assert_eq!(lanes(HEADER, "{\\an8}Top dialogue", 100.0).best(), 100);
        assert_eq!(
            lanes(HEADER, "{\\pos(960,80)}A positioned sign", 100.0).best(),
            100
        );
        assert_eq!(
            lanes(HEADER, "{\\move(900,50,900,1000)}Moving sign", 100.0).0,
            7
        );
        assert_eq!(lanes("", "Plain text", 100.0).best(), 0);
        assert_eq!(lanes("", "", 100.0).best(), 0);
    }
}
