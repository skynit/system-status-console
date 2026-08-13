mod commands;

use tauri::Manager;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitialRoute {
    Dashboard,
    Applications,
    ApplicationsUsage,
    ApplicationsUsageWeekly,
    Network,
    Remote,
    RemoteFtp,
    RemoteSmb,
    Transfers,
    Memos,
    MemosList,
    Settings,
}

impl InitialRoute {
    const fn fragment(self) -> &'static str {
        match self {
            Self::Dashboard => "/",
            Self::Applications => "/applications",
            Self::ApplicationsUsage => "/applications?panel=usage",
            Self::ApplicationsUsageWeekly => "/applications?panel=usage&period=weekly",
            Self::Network => "/network",
            Self::Remote => "/remote",
            Self::RemoteFtp => "/remote?protocol=ftp",
            Self::RemoteSmb => "/remote?protocol=smb",
            Self::Transfers => "/transfers",
            Self::Memos => "/memos",
            Self::MemosList => "/memos?view=list",
            Self::Settings => "/settings",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "dashboard" => Ok(Self::Dashboard),
            "applications" => Ok(Self::Applications),
            "applications-usage" => Ok(Self::ApplicationsUsage),
            "applications-usage-weekly" => Ok(Self::ApplicationsUsageWeekly),
            "network" => Ok(Self::Network),
            "remote" => Ok(Self::Remote),
            "remote-ftp" => Ok(Self::RemoteFtp),
            "remote-smb" => Ok(Self::RemoteSmb),
            "transfers" => Ok(Self::Transfers),
            "memos" => Ok(Self::Memos),
            "memos-list" => Ok(Self::MemosList),
            "settings" => Ok(Self::Settings),
            _ => Err(format!("unsupported initial route: {value}")),
        }
    }
}

fn initial_route_from_args(
    args: impl IntoIterator<Item = String>,
) -> Result<Option<InitialRoute>, String> {
    let mut args = args.into_iter().skip(1);
    let mut route = None;
    while let Some(argument) = args.next() {
        if argument != "--route" {
            continue;
        }
        if route.is_some() {
            return Err("initial route specified more than once".to_owned());
        }
        let value = args
            .next()
            .ok_or_else(|| "missing value for --route".to_owned())?;
        route = Some(InitialRoute::parse(&value)?);
    }
    Ok(route)
}

pub fn run(context: tauri::Context<tauri::Wry>) {
    let initial_route = initial_route_from_args(std::env::args())
        .unwrap_or_else(|error| panic!("invalid localdesk-desktop arguments: {error}"));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            if let Some(route) = initial_route {
                let main = app
                    .get_webview_window("main")
                    .ok_or("main webview window is unavailable")?;
                let mut url = main.url()?;
                url.set_fragment(Some(route.fragment()));
                main.navigate(url)?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::appd_health,
            commands::network_snapshot,
            commands::notes_autosave,
            commands::notes_delete,
            commands::notes_export,
            commands::notes_get,
            commands::notes_list,
            commands::notes_restore,
            commands::notes_upsert,
            commands::remote_capabilities,
            commands::remote_profile,
            commands::remote_session,
            commands::remote_terminal,
            commands::remote_terminal_stream,
            commands::secret,
            commands::telemetry_snapshot,
            commands::transfer_cancel,
            commands::transfer_enqueue,
            commands::transfer_get,
            commands::transfer_list,
            commands::transfer_pick_download_destination,
            commands::transfer_pick_upload_source,
            commands::transfer_resolve_conflict,
            commands::transfer_retry,
            commands::speedtest_basic,
            commands::speedtest_cancel,
            commands::speedtest_deep,
            commands::usage_summary,
            commands::system_info,
        ])
        .run(context)
        .expect("error while running localdesk-desktop");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn startup_route_accepts_only_known_application_routes() {
        assert_eq!(
            initial_route_from_args(args(&["localdesk-desktop", "--route", "applications"])),
            Ok(Some(InitialRoute::Applications))
        );
        assert_eq!(InitialRoute::Applications.fragment(), "/applications");
        assert_eq!(
            initial_route_from_args(args(&[
                "localdesk-desktop",
                "--route",
                "applications-usage",
            ])),
            Ok(Some(InitialRoute::ApplicationsUsage))
        );
        assert_eq!(
            InitialRoute::ApplicationsUsage.fragment(),
            "/applications?panel=usage"
        );
        assert_eq!(
            initial_route_from_args(args(&[
                "localdesk-desktop",
                "--route",
                "applications-usage-weekly",
            ])),
            Ok(Some(InitialRoute::ApplicationsUsageWeekly))
        );
        assert_eq!(
            InitialRoute::ApplicationsUsageWeekly.fragment(),
            "/applications?panel=usage&period=weekly"
        );
        assert_eq!(
            initial_route_from_args(args(&["localdesk-desktop", "--route", "remote-ftp"])),
            Ok(Some(InitialRoute::RemoteFtp))
        );
        assert_eq!(InitialRoute::RemoteFtp.fragment(), "/remote?protocol=ftp");
        assert_eq!(
            initial_route_from_args(args(&["localdesk-desktop", "--route", "remote-smb"])),
            Ok(Some(InitialRoute::RemoteSmb))
        );
        assert_eq!(InitialRoute::RemoteSmb.fragment(), "/remote?protocol=smb");
        assert_eq!(
            initial_route_from_args(args(&["localdesk-desktop", "--route", "memos-list"])),
            Ok(Some(InitialRoute::MemosList))
        );
        assert_eq!(InitialRoute::MemosList.fragment(), "/memos?view=list");
        assert!(
            initial_route_from_args(args(&["localdesk-desktop", "--route", "unknown"])).is_err()
        );
    }

    #[test]
    fn startup_route_rejects_missing_and_duplicate_values() {
        assert!(initial_route_from_args(args(&["localdesk-desktop", "--route"])).is_err());
        assert!(
            initial_route_from_args(args(&[
                "localdesk-desktop",
                "--route",
                "network",
                "--route",
                "memos",
            ]))
            .is_err()
        );
    }

    #[test]
    fn startup_route_is_optional_and_ignores_unrelated_runtime_arguments() {
        assert_eq!(
            initial_route_from_args(args(&["localdesk-desktop", "--gtk-debug", "all"])),
            Ok(None)
        );
    }
}
