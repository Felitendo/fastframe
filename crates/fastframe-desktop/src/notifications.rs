use std::sync::OnceLock;

#[cfg(target_os = "macos")]
pub(super) fn allowed() -> bool {
    static BUNDLED: OnceLock<bool> = OnceLock::new();
    allowed_with(
        &BUNDLED,
        || match mac_usernotifications::check_bundle() {
            Ok(()) => true,
            Err(error) => {
                log::debug!("no notifications outside the application bundle: {error}");
                false
            }
        },
        || match mac_usernotifications::blocking::request_auth() {
            Ok(granted) => granted,
            Err(error) => {
                log::debug!("could not ask for notification permission: {error}");
                false
            }
        },
    )
}

fn allowed_with(
    bundled: &OnceLock<bool>,
    check_bundle: impl FnOnce() -> bool,
    request_auth: impl FnOnce() -> bool,
) -> bool {
    *bundled.get_or_init(check_bundle) && request_auth()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn unbundled_runs_never_reach_the_native_permission_api() {
        let bundled = OnceLock::new();
        assert!(!allowed_with(&bundled, || false, || panic!("unbundled")));
        assert!(!allowed_with(
            &bundled,
            || panic!("bundle check is cached"),
            || panic!("unbundled")
        ));
    }

    #[test]
    fn permission_changes_apply_without_rechecking_the_bundle() {
        let bundled = OnceLock::new();
        let checked = Cell::new(0);
        let requested = Cell::new(0);
        for granted in [false, true, false, true] {
            assert_eq!(
                allowed_with(
                    &bundled,
                    || {
                        checked.set(checked.get() + 1);
                        true
                    },
                    || {
                        requested.set(requested.get() + 1);
                        granted
                    }
                ),
                granted
            );
        }
        assert_eq!(checked.get(), 1);
        assert_eq!(requested.get(), 4);
    }

    #[test]
    fn the_bundle_is_checked_before_requesting_permission() {
        let bundled = OnceLock::new();
        assert!(allowed_with(
            &bundled,
            || true,
            || {
                assert_eq!(bundled.get(), Some(&true));
                true
            }
        ));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_platforms_do_not_claim_macos_permission() {
        assert!(!crate::macos_notifications_allowed());
    }
}
