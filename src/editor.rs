//! Open a project folder in a code editor (VS Code or Cursor).
//!
//! The command to run is built by a pure function so each platform's form is testable. Only absolute paths are accepted, so a
//! path can never be mistaken for an option by the editor's launcher (`open`, `code`, ...).

/// The editors we know how to launch, with how each is started per platform.
struct Editor {
    id: &'static str,
    /// macOS application name, for `open -a`.
    mac_app: &'static str,
    /// Command line launcher on Windows/Linux (`.cmd` shim on Windows).
    command: &'static str,
}

const EDITORS: &[Editor] = &[
    Editor { id: "code", mac_app: "Visual Studio Code", command: "code" },
    Editor { id: "cursor", mac_app: "Cursor", command: "cursor" },
];

/// Whether `path` is absolute on `os`. Judged by the target platform's rules, not the machine running the check, so the answer for a
/// Windows path is the same everywhere.
fn is_absolute_for(os: &str, path: &str) -> bool {
    if os == "windows" {
        let b = path.as_bytes();
        let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
        drive || path.starts_with(r"\\")
    } else {
        path.starts_with('/')
    }
}

/// `(program, args)` that opens `path` in `editor_id` on `os` (a `std::env::consts::OS` value).
pub fn editor_command(os: &str, editor_id: &str, path: &str) -> Result<(String, Vec<String>), String> {
    let editor = EDITORS
        .iter()
        .find(|e| e.id == editor_id)
        .ok_or_else(|| format!("unknown editor {editor_id:?}; expected one of: {}", EDITORS.iter().map(|e| e.id).collect::<Vec<_>>().join(", ")))?;
    if !is_absolute_for(os, path) {
        return Err(format!("{path:?} is not an absolute path"));
    }
    Ok(match os {
        "macos" => ("open".into(), vec!["-a".into(), editor.mac_app.into(), path.into()]),
        "windows" => (format!("{}.cmd", editor.command), vec![path.into()]),
        _ => (editor.command.into(), vec![path.into()]),
    })
}

pub fn open_in_editor(editor_id: &str, path: &str) -> Result<(), String> {
    let (program, args) = editor_command(std::env::consts::OS, editor_id, path)?;
    std::process::Command::new(&program)
        .args(&args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("couldn't start {program}: {e} (is the editor installed?)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_uses_open_with_the_application_name() {
        assert_eq!(
            editor_command("macos", "code", "/Users/me/My Projects/demo").unwrap(),
            ("open".to_string(), vec!["-a".to_string(), "Visual Studio Code".to_string(), "/Users/me/My Projects/demo".to_string()])
        );
        assert_eq!(editor_command("macos", "cursor", "/p").unwrap().1[1], "Cursor");
    }

    #[test]
    fn windows_uses_the_cmd_shim_and_keeps_a_path_with_spaces_as_one_argument() {
        let (program, args) = editor_command("windows", "code", r"C:\Users\me\My Projects\demo").unwrap();
        assert_eq!(program, "code.cmd");
        assert_eq!(args, vec![r"C:\Users\me\My Projects\demo".to_string()]);
        assert_eq!(editor_command("windows", "cursor", r"C:\p").unwrap().0, "cursor.cmd");
    }

    #[test]
    fn linux_runs_the_launcher_directly() {
        assert_eq!(editor_command("linux", "cursor", "/home/me/demo").unwrap(), ("cursor".to_string(), vec!["/home/me/demo".to_string()]));
    }

    #[test]
    fn only_known_editors_are_accepted() {
        let err = editor_command("linux", "rm", "/p").unwrap_err();
        assert!(err.contains("unknown editor") && err.contains("code, cursor"));
    }

    #[test]
    fn relative_paths_and_option_lookalikes_are_refused() {
        for os in ["macos", "linux", "windows"] {
            for bad in ["demo", "./demo", "../demo", "-n", "--new-window", "", r"..\demo", r"C:demo", "/"
                .trim_end_matches('/')] {
                assert!(editor_command(os, "code", bad).is_err(), "{bad:?} should be refused on {os}");
            }
        }
    }

    #[test]
    fn absoluteness_follows_the_target_platform() {
        assert!(is_absolute_for("windows", r"C:\Users\me") && is_absolute_for("windows", "D:/work/demo") && is_absolute_for("windows", r"\\server\share\p"));
        assert!(!is_absolute_for("windows", "/home/me"), "a unix path is not absolute on windows");
        assert!(is_absolute_for("linux", "/home/me") && is_absolute_for("macos", "/Users/me"));
        assert!(!is_absolute_for("linux", r"C:\Users\me"));
    }
}
