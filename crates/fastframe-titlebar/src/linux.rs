//! GNOME's button layout and the icon theme's window icons.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::Layout;

pub(crate) const MINIMIZE: usize = 0;
pub(crate) const MAXIMIZE: usize = 1;
pub(crate) const RESTORE: usize = 2;
pub(crate) const CLOSE: usize = 3;

/// The icon theme names, by the indices above.
const NAMES: [&str; 4] = [
    "window-minimize-symbolic",
    "window-maximize-symbolic",
    "window-restore-symbolic",
    "window-close-symbolic",
];

const URIS: [&str; 4] = [
    "bytes://fastframe-titlebar-theme-minimize.svg",
    "bytes://fastframe-titlebar-theme-maximize.svg",
    "bytes://fastframe-titlebar-theme-restore.svg",
    "bytes://fastframe-titlebar-theme-close.svg",
];

static LAYOUT: OnceLock<Layout> = OnceLock::new();
static ICONS: OnceLock<[Option<Vec<u8>>; 4]> = OnceLock::new();

pub(crate) fn init() {
    LAYOUT.get_or_init(read_layout);
    ICONS.get_or_init(read_icons);
}

pub(crate) fn layout() -> &'static Layout {
    LAYOUT.get_or_init(Layout::standard)
}

pub(crate) fn icon(index: usize) -> Option<egui::Image<'static>> {
    let bytes: &'static [u8] = ICONS.get()?[index].as_deref()?;
    Some(egui::Image::from_bytes(URIS[index], bytes))
}

/// GNOME's layout, or close alone when it has none (GNOME's default). Other
/// desktops draw all three.
fn read_layout() -> Layout {
    if !fastframe_desktop::is_gnome() {
        return Layout::standard();
    }
    fastframe_desktop::setting("org.gnome.desktop.wm.preferences", "button-layout")
        .and_then(|layout| Layout::from_gnome(&layout))
        .filter(|layout| !layout.left.is_empty() || !layout.right.is_empty())
        .unwrap_or_else(|| Layout {
            left: Vec::new(),
            right: vec![crate::Button::Close],
        })
}

fn read_icons() -> [Option<Vec<u8>>; 4] {
    let theme = fastframe_desktop::setting("org.gnome.desktop.interface", "icon-theme")
        .map(|theme| theme.trim().trim_matches('\'').to_owned())
        .filter(|theme| !theme.contains('/') && theme != "." && theme != "..")
        .unwrap_or_else(|| "Adwaita".into());
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dirs) = directories::BaseDirs::new() {
        roots.push(dirs.data_dir().join("icons"));
        roots.push(dirs.home_dir().join(".icons"));
    }
    roots.extend(
        std::env::split_paths(
            &std::env::var_os("XDG_DATA_DIRS")
                .unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
        )
        .map(|root| root.join("icons")),
    );
    NAMES.map(|name| find_icon(&roots, &theme, name))
}

fn find_icon(roots: &[PathBuf], theme: &str, name: &str) -> Option<Vec<u8>> {
    for theme in [theme, "Adwaita"] {
        for root in roots {
            for folder in [
                "symbolic/ui",
                "symbolic/actions",
                "scalable/actions",
                "16x16/actions",
            ] {
                let path = root.join(theme).join(folder).join(format!("{name}.svg"));
                if let Ok(svg) = std::fs::read_to_string(path)
                    && let Some(white) = tintable(&svg)
                {
                    return Some(white);
                }
            }
        }
    }
    None
}

/// GTK recolours symbolic icons itself; an SVG loader does not. Filling
/// every shape white lets egui's tint colour the glyph.
fn tintable(svg: &str) -> Option<Vec<u8>> {
    svg.contains("</svg>").then(|| {
        let fill = concat!(
            "<style>path,rect,circle,ellipse,polygon,polyline",
            " { fill: white !important; }</style></svg>",
        );
        svg.replace("</svg>", fill).into_bytes()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_icon_is_found_in_the_first_theme_that_has_it() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().join("empty"), root.path().to_owned()];
        let actions = root.path().join("Adwaita/symbolic/ui");
        std::fs::create_dir_all(&actions).unwrap();
        std::fs::write(
            actions.join("window-close-symbolic.svg"),
            "<svg><path d=\"M0 0\"/></svg>",
        )
        .unwrap();
        std::fs::write(actions.join("window-minimize-symbolic.svg"), "not svg").unwrap();
        let close = find_icon(&roots, "Missing", "window-close-symbolic").unwrap();
        let close = String::from_utf8(close).unwrap();
        assert!(close.contains("fill: white !important"), "{close}");
        assert!(close.ends_with("</style></svg>"), "{close}");
        assert_eq!(
            find_icon(&roots, "Missing", "window-minimize-symbolic"),
            None
        );
        assert_eq!(
            find_icon(&roots, "Missing", "window-restore-symbolic"),
            None
        );
    }
}
