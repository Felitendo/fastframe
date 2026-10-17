//! The launch agent a macOS bundle carries to start its app hidden.

/// See [`crate::macos_agent_plist`].
pub(crate) fn agent(bundle_id: &str, hidden_argument: &str) -> String {
    let id = escape(bundle_id);
    let hidden = escape(hidden_argument);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{id}.autostart</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/open</string>
        <string>-g</string>
        <string>-b</string>
        <string>{id}</string>
        <string>--args</string>
        <string>{hidden}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
</dict>
</plist>
"#
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_opens_the_bundle_hidden_through_launch_services() {
        let plist = agent("rocks.spotifast.Spotifast", "--start-hidden");
        assert!(plist.contains("<string>rocks.spotifast.Spotifast.autostart</string>"));
        assert!(plist.contains(
            "<string>/usr/bin/open</string>\n        <string>-g</string>\n        \
             <string>-b</string>\n        <string>rocks.spotifast.Spotifast</string>\n        \
             <string>--args</string>\n        <string>--start-hidden</string>"
        ));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(
            !plist.contains("KeepAlive"),
            "open exits; nothing restarts it"
        );
    }

    #[test]
    fn the_agent_escapes_what_it_names() {
        assert!(agent("a&b", "--x<y>").contains("<string>--x&lt;y&gt;</string>"));
        assert!(agent("a&b", "-x").contains("<string>a&amp;b</string>"));
    }
}
