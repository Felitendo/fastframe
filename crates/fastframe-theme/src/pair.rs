//! A light and a dark palette that follow the system's appearance.

use crate::{Base, Catalog, CustomTheme, PaletteBase, Status, omarchy};

/// The palette a [`Pair`] shows for one appearance: a palette file, with
/// the colours last read from it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(bound(
    serialize = "P: serde::Serialize",
    deserialize = "P: serde::de::DeserializeOwned"
))]
pub struct Slot<P> {
    /// The file name in the themes directory, such as `Nord.json`.
    pub filename: String,
    /// The colours last accepted from the file, so the first frame after a
    /// restart is right and a file that disappears keeps its look. A
    /// damaged cache reads as `None` and leaves the choice in place.
    #[serde(
        default,
        deserialize_with = "read_cached_palette",
        skip_serializing_if = "Option::is_none"
    )]
    pub palette: Option<P>,
}

/// A palette for light mode and one for dark mode, shown as the system's
/// appearance changes. An empty slot is the app's own palette for that
/// base ([`crate::Palette::base`]), so `Pair::default()` is the app's usual
/// "follow system".
///
/// The pair lives in the app's settings and serializes as
/// `{ "light": { "filename": .., "palette": .. }, "dark": .. }`, leaving out
/// empty slots. Each slot holds only palettes of its own base: a light
/// palette for light mode, a dark one for dark mode, so the widgets'
/// light or dark style always matches the system's and the colours. The
/// live Omarchy palette is never a slot; it follows the desktop by itself.
///
/// ```
/// # use egui::Color32;
/// # #[derive(Clone, Debug, PartialEq)]
/// # struct Colors { dark: bool, window: Color32 }
/// # impl fastframe_theme::Palette for Colors {
/// #     fn base(base: fastframe_theme::Base) -> Self {
/// #         let dark = base == fastframe_theme::Base::Dark;
/// #         Colors { dark, window: if dark { Color32::BLACK } else { Color32::WHITE } }
/// #     }
/// #     fn set(&mut self, name: &str, color: Color32) -> bool {
/// #         if name == "window" { self.window = color; true } else { false }
/// #     }
/// # }
/// # impl fastframe_theme::PaletteBase for Colors {
/// #     fn palette_base(&self) -> fastframe_theme::Base {
/// #         if self.dark { fastframe_theme::Base::Dark } else { fastframe_theme::Base::Light }
/// #     }
/// # }
/// use fastframe_theme::{Base, CustomTheme, Pair, Palette as _};
///
/// let latte = CustomTheme {
///     filename: "Latte.json".to_owned(),
///     palette: fastframe_theme::parse_palette::<Colors>(
///         r##"{"base":"light","colors":{"window":"#eff1f5"}}"##,
///     )?,
/// };
/// let mut pair = Pair::default();
/// assert!(pair.set(Base::Light, Some(&latte)));
/// assert!(!pair.set(Base::Dark, Some(&latte)), "a light palette is not for dark mode");
///
/// // Each frame, with the system's appearance:
/// let dark = false;
/// assert_eq!(pair.palette(Base::from_dark(dark)), latte.palette);
/// assert_eq!(pair.palette(Base::Dark), Colors::base(Base::Dark));
/// # Ok::<(), String>(())
/// ```
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(bound(
    serialize = "P: serde::Serialize",
    deserialize = "P: serde::de::DeserializeOwned"
))]
pub struct Pair<P> {
    /// The palette for light mode; `None` for the app's own.
    #[serde(
        default,
        deserialize_with = "read_slot",
        skip_serializing_if = "Option::is_none"
    )]
    pub light: Option<Slot<P>>,
    /// The palette for dark mode; `None` for the app's own.
    #[serde(
        default,
        deserialize_with = "read_slot",
        skip_serializing_if = "Option::is_none"
    )]
    pub dark: Option<Slot<P>>,
}

impl<P> Default for Pair<P> {
    fn default() -> Self {
        Self {
            light: None,
            dark: None,
        }
    }
}

/// Whether `theme` may fill the slot for `base`: a palette built on that
/// base, and not the live Omarchy palette.
fn eligible<P: PaletteBase>(base: Base, theme: &CustomTheme<P>) -> bool {
    theme.filename != omarchy::FILENAME && theme.palette.palette_base() == base
}

impl<P> Pair<P> {
    /// Whether both slots hold the app's own palettes, as for
    /// `#[serde(skip_serializing_if = "fastframe_theme::Pair::is_default")]`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.light.is_none() && self.dark.is_none()
    }
}

impl<P: PaletteBase> Pair<P> {
    /// The slot for `base`.
    #[must_use]
    pub fn slot(&self, base: Base) -> Option<&Slot<P>> {
        match base {
            Base::Light => self.light.as_ref(),
            Base::Dark => self.dark.as_ref(),
        }
    }

    /// The chosen file for `base`, or `None` for the app's own palette.
    #[must_use]
    pub fn filename(&self, base: Base) -> Option<&str> {
        self.slot(base).map(|slot| slot.filename.as_str())
    }

    fn slot_mut(&mut self, base: Base) -> &mut Option<Slot<P>> {
        match base {
            Base::Light => &mut self.light,
            Base::Dark => &mut self.dark,
        }
    }

    /// Chooses `theme` for `base`, or the app's own palette with `None`.
    /// Returns `false`, changing nothing, for a palette of the other base
    /// or the live Omarchy palette.
    pub fn set(&mut self, base: Base, theme: Option<&CustomTheme<P>>) -> bool {
        let slot = match theme {
            Some(theme) if !eligible(base, theme) => return false,
            Some(theme) => Some(Slot {
                filename: theme.filename.clone(),
                palette: Some(theme.palette.clone()),
            }),
            None => None,
        };
        *self.slot_mut(base) = slot;
        true
    }

    /// The palette to show while the system's appearance is `base`: the
    /// chosen file's colours, as last accepted, or the app's own palette
    /// when the slot is empty or its file was never read.
    #[must_use]
    pub fn palette(&self, base: Base) -> P {
        self.slot(base)
            .and_then(|slot| slot.palette.clone())
            .unwrap_or_else(|| P::base(base))
    }

    /// Takes each chosen file's colours from a finished scan, so an edited
    /// file shows its new colours. A file the catalogue no longer lists, or
    /// that now names the other base, keeps its last accepted colours.
    /// Returns `true` when a slot changed, so the app saves its settings.
    pub fn refresh(&mut self, catalog: &Catalog<P>) -> bool {
        let mut changed = false;
        for base in [Base::Light, Base::Dark] {
            if let Some(slot) = self.slot_mut(base)
                && let Some(theme) = catalog.find(&slot.filename)
                && eligible(base, theme)
                && slot.palette.as_ref() != Some(&theme.palette)
            {
                slot.palette = Some(theme.palette.clone());
                changed = true;
            }
        }
        changed
    }

    /// The chosen files, for [`Catalog::start`] to read first.
    pub fn filenames(&self) -> impl Iterator<Item = String> + '_ {
        [&self.light, &self.dark]
            .into_iter()
            .flatten()
            .map(|slot| slot.filename.clone())
    }

    /// Whether the file chosen for `base` is missing from the catalogue, or
    /// no longer a palette of that base. `false` for an empty slot.
    #[must_use]
    pub fn unavailable(&self, base: Base, catalog: &Catalog<P>) -> bool {
        self.filename(base).is_some_and(|filename| {
            !catalog
                .find(filename)
                .is_some_and(|theme| eligible(base, theme))
        })
    }

    /// What to say under the slot for `base`, as [`Catalog::status`] does
    /// for a single palette.
    #[must_use]
    pub fn status(&self, base: Base, catalog: &Catalog<P>) -> Option<Status> {
        if catalog.loading() {
            return Some(Status::Loading);
        }
        if self.unavailable(base, catalog) {
            return Some(Status::SelectedUnavailable);
        }
        catalog.status(None)
    }
}

impl<P: PaletteBase> Catalog<P> {
    /// The palettes a [`Pair`] offers for `base`, in picker order: those
    /// built on that base, without the live Omarchy palette.
    pub fn pair_themes(&self, base: Base) -> impl Iterator<Item = &CustomTheme<P>> {
        self.themes()
            .iter()
            .filter(move |theme| eligible(base, theme))
    }
}

fn read_cached_palette<'de, D, P>(deserializer: D) -> Result<Option<P>, D::Error>
where
    D: serde::Deserializer<'de>,
    P: serde::de::DeserializeOwned,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    match serde_json::from_value(value) {
        Ok(palette) => Ok(Some(palette)),
        Err(error) => {
            log::warn!("ignoring an unreadable cached palette: {error}");
            Ok(None)
        }
    }
}

/// Reads a slot, treating one without a usable filename as empty so the
/// rest of the settings still load.
fn read_slot<'de, D, P>(deserializer: D) -> Result<Option<Slot<P>>, D::Error>
where
    D: serde::Deserializer<'de>,
    P: serde::de::DeserializeOwned,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    match serde_json::from_value(value) {
        Ok(slot) => Ok(Some(slot)),
        Err(error) => {
            log::warn!("ignoring an unreadable theme choice: {error}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Palette as _;
    use crate::test_palette::Colors;
    use egui::Color32;

    fn theme(filename: &str, base: Base, window: Color32) -> CustomTheme<Colors> {
        let mut palette = Colors::base(base);
        palette.set("window", window);
        CustomTheme {
            filename: filename.into(),
            palette,
        }
    }

    const LATTE: Color32 = Color32::from_rgb(0xef, 0xf1, 0xf5);
    const MOCHA: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x2e);

    fn latte() -> CustomTheme<Colors> {
        theme("Latte.json", Base::Light, LATTE)
    }

    fn mocha() -> CustomTheme<Colors> {
        theme("Mocha.json", Base::Dark, MOCHA)
    }

    #[test]
    fn the_default_pair_is_the_apps_own_light_and_dark() {
        let pair = Pair::<Colors>::default();
        assert!(pair.is_default());
        assert_eq!(pair.palette(Base::Light), Colors::base(Base::Light));
        assert_eq!(pair.palette(Base::Dark), Colors::base(Base::Dark));
        assert_eq!(pair.filenames().count(), 0);
    }

    #[test]
    fn each_appearance_shows_its_own_palette() {
        let mut pair = Pair::default();
        assert!(pair.set(Base::Light, Some(&latte())));
        assert!(pair.set(Base::Dark, Some(&mocha())));
        assert_eq!(pair.palette(Base::from_dark(false)).get("window"), LATTE);
        assert_eq!(pair.palette(Base::from_dark(true)).get("window"), MOCHA);
        assert_eq!(pair.filename(Base::Light), Some("Latte.json"));
        assert_eq!(
            pair.filenames().collect::<Vec<_>>(),
            ["Latte.json", "Mocha.json"]
        );
        assert!(pair.set(Base::Light, None));
        assert_eq!(pair.palette(Base::Light), Colors::base(Base::Light));
        assert_eq!(pair.palette(Base::Dark).get("window"), MOCHA);
    }

    #[test]
    fn a_slot_takes_only_palettes_of_its_base() {
        let mut pair = Pair::default();
        assert!(!pair.set(Base::Dark, Some(&latte())));
        assert!(!pair.set(Base::Light, Some(&mocha())));
        let omarchy = theme(omarchy::FILENAME, Base::Dark, MOCHA);
        assert!(!pair.set(Base::Dark, Some(&omarchy)));
        assert!(pair.is_default());

        let catalog = Catalog::preview(vec![latte(), mocha(), omarchy], true);
        let names = |base| {
            catalog
                .pair_themes(base)
                .map(|theme| theme.filename.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(Base::Light), ["Latte.json"]);
        assert_eq!(names(Base::Dark), ["Mocha.json"]);
    }

    #[test]
    fn a_missing_file_keeps_its_last_colours_and_says_so() {
        let mut pair = Pair::default();
        pair.set(Base::Dark, Some(&mocha()));
        let empty = Catalog::<Colors>::preview(Vec::new(), false);
        assert!(!pair.refresh(&empty));
        assert_eq!(pair.palette(Base::Dark).get("window"), MOCHA);
        assert!(pair.unavailable(Base::Dark, &empty));
        assert_eq!(
            pair.status(Base::Dark, &empty),
            Some(Status::SelectedUnavailable)
        );
        assert!(!pair.unavailable(Base::Light, &empty), "an empty slot");
        assert_eq!(pair.status(Base::Light, &empty), None);

        // Without a cache (a damaged one), the app's own dark palette shows.
        pair.dark.as_mut().unwrap().palette = None;
        assert_eq!(pair.palette(Base::Dark), Colors::base(Base::Dark));
        // And the file coming back restores it.
        let listed = Catalog::preview(vec![mocha()], false);
        assert!(pair.refresh(&listed));
        assert_eq!(pair.palette(Base::Dark).get("window"), MOCHA);
        assert_eq!(pair.status(Base::Dark, &listed), None);
    }

    #[test]
    fn an_edited_file_shows_its_new_colours_unless_it_changed_base() {
        let mut pair = Pair::default();
        pair.set(Base::Dark, Some(&mocha()));
        let edited = theme("Mocha.json", Base::Dark, Color32::RED);
        assert!(pair.refresh(&Catalog::preview(vec![edited], false)));
        assert_eq!(pair.palette(Base::Dark).get("window"), Color32::RED);
        assert!(!pair.refresh(&Catalog::preview(
            vec![theme("Mocha.json", Base::Dark, Color32::RED)],
            false
        )));

        let turned_light = Catalog::preview(vec![theme("Mocha.json", Base::Light, LATTE)], false);
        assert!(!pair.refresh(&turned_light));
        assert_eq!(pair.palette(Base::Dark).get("window"), Color32::RED);
        assert!(pair.unavailable(Base::Dark, &turned_light));
    }

    #[test]
    fn pairs_round_trip_and_damage_stays_contained() {
        let mut pair = Pair::default();
        pair.set(Base::Light, Some(&latte()));
        let text = serde_json::to_string(&pair).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            value.get("dark").is_none(),
            "empty slots are left out: {text}"
        );
        assert_eq!(serde_json::from_str::<Pair<Colors>>(&text).unwrap(), pair);
        assert_eq!(
            serde_json::from_str::<Pair<Colors>>("{}").unwrap(),
            Pair::default()
        );

        // A damaged cache keeps the choice; a damaged choice empties the slot.
        let damaged: Pair<Colors> = serde_json::from_str(
            r#"{"light":{"filename":"Latte.json","palette":"broken"},"dark":{"palette":{}}}"#,
        )
        .unwrap();
        assert_eq!(damaged.filename(Base::Light), Some("Latte.json"));
        assert_eq!(damaged.light.as_ref().unwrap().palette, None);
        assert_eq!(damaged.dark, None);
    }
}
