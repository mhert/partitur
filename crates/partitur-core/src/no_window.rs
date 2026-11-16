//! Keeps child processes from opening a console window.
//!
//! Partitur is a windowed program, so on Windows every console program it starts would
//! otherwise flash its own console window. [`no_window`] asks Windows not to create one. On
//! other platforms it changes nothing.

/// The process creation flag that starts a console program without a console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Makes `command` start without a console window on Windows and returns it for chaining; on
/// other platforms `command` is returned unchanged.
pub fn no_window(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW)
    }
    #[cfg(not(windows))]
    {
        command
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::no_window;

    #[test]
    fn no_window_returns_the_same_command_and_keeps_its_program() {
        let mut command = Command::new("x");
        assert_eq!(no_window(&mut command).get_program(), "x");
    }
}
