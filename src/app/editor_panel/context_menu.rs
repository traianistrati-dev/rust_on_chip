//! Right-click context menu for the code editor.
//!
//! Lists every editor command together with its keyboard shortcut. Clicking a
//! row returns an [`EditorAction`]; the caller (`mod.rs`) maps it onto the same
//! key flags the shortcut would set, so both paths share one implementation.

use eframe::egui;
use egui_phosphor::regular as ph;

/// A command chosen from the editor right-click menu.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum EditorAction {
    Cut,
    DeleteLine,
    DuplicateLine,
    Comment,
    /// Wrap the selected lines in one `/* … */` (Ctrl+Shift+/).
    BlockComment,
    /// UPPER / lower case of the selection, or of the identifier at the caret
    /// (Ctrl+U).
    ToggleCase,
    MoveUp,
    MoveDown,
    Format,
    /// Collapse every function body, or expand everything (Ctrl+Shift+Q).
    ToggleFoldAll,
    Rename,
    GoToDef,
    GoToImpl,
    /// Add the selection (or the identifier at the caret) to the Debug tab's
    /// Watch pane.
    AddWatch,
    /// Time the function around the caret on the board (Profile tab, Timing).
    TimeFunction,
    /// Time the statements the selection touches on the board.
    TimeLines,
    Completion,
    Find,
    Replace,
    FindInProject,
    ReplaceInProject,
    Copy,
    SelectAll,
    /// Select + copy the innermost `{ … }` block around the caret (Ctrl+[).
    SelectBlock,
    /// Move the selected lines into a new function (Ctrl+Alt+Insert).
    ExtractFn,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// Switch to the next file in the MRU history (Ctrl+Tab).
    NextFile,
    /// Switch to the previous file in the MRU history (Ctrl+Shift+Tab).
    PrevFile,
}

/// Render the editor context menu. `is_rs` enables the rust-analyzer-backed
/// items (format / rename / go-to-definition); `is_cargo` relabels completion
/// for Cargo.toml. Returns the clicked action, if any.
pub(super) fn editor_menu(ui: &mut egui::Ui, is_rs: bool, is_cargo: bool) -> Option<EditorAction> {
    ui.set_min_width(248.0);
    let mut chosen: Option<EditorAction> = None;

    let mut item =
        |ui: &mut egui::Ui, icon: &str, label: &str, shortcut: &str, action: EditorAction| {
            let btn = egui::Button::new(egui::RichText::new(format!("{icon}  {label}")).size(12.0))
                .shortcut_text(egui::RichText::new(shortcut).size(11.0))
                .min_size(egui::vec2(ui.available_width(), 0.0));
            if ui.add(btn).clicked() {
                chosen = Some(action);
                ui.close();
            }
        };

    item(ui, ph::SCISSORS, "Cut", "Ctrl+X", EditorAction::Cut);
    item(
        ui,
        ph::SCISSORS,
        "Cut line",
        "Ctrl+Shift+X",
        EditorAction::DeleteLine,
    );
    item(
        ui,
        ph::COPY_SIMPLE,
        "Duplicate line",
        "Ctrl+D",
        EditorAction::DuplicateLine,
    );
    item(
        ui,
        ph::CODE,
        "Toggle comment",
        "Ctrl+/",
        EditorAction::Comment,
    );
    if is_rs {
        item(
            ui,
            ph::BRACKETS_CURLY,
            "Block comment selection",
            "Ctrl+Shift+/",
            EditorAction::BlockComment,
        );
    }
    item(
        ui,
        ph::TEXT_AA,
        "Toggle case",
        "Ctrl+U",
        EditorAction::ToggleCase,
    );
    item(
        ui,
        ph::ARROW_UP,
        "Move line up",
        "Ctrl+Up",
        EditorAction::MoveUp,
    );
    item(
        ui,
        ph::ARROW_DOWN,
        "Move line down",
        "Ctrl+Down",
        EditorAction::MoveDown,
    );

    ui.separator();
    item(
        ui,
        ph::ARROW_RIGHT,
        "Next file (recent)",
        "Ctrl+Tab",
        EditorAction::NextFile,
    );
    item(
        ui,
        ph::ARROW_LEFT,
        "Previous file (recent)",
        "Ctrl+Shift+Tab",
        EditorAction::PrevFile,
    );

    ui.separator();
    item(
        ui,
        ph::MAGNIFYING_GLASS,
        "Find",
        "Ctrl+F",
        EditorAction::Find,
    );
    item(
        ui,
        ph::ARROWS_LEFT_RIGHT,
        "Replace",
        "Ctrl+H",
        EditorAction::Replace,
    );
    item(
        ui,
        ph::MAGNIFYING_GLASS_PLUS,
        "Find in project",
        "Ctrl+Shift+F",
        EditorAction::FindInProject,
    );
    item(
        ui,
        ph::ARROWS_CLOCKWISE,
        "Replace in project",
        "Ctrl+Shift+H",
        EditorAction::ReplaceInProject,
    );

    if is_rs {
        ui.separator();
        item(
            ui,
            ph::ARROWS_IN_LINE_VERTICAL,
            "Collapse / expand all functions",
            "Ctrl+Shift+Q",
            EditorAction::ToggleFoldAll,
        );
        item(
            ui,
            ph::TEXT_INDENT,
            "Format code",
            "Shift+Alt+F",
            EditorAction::Format,
        );
        item(
            ui,
            ph::SCISSORS,
            "Extract selection into a function",
            "Ctrl+Alt+M",
            EditorAction::ExtractFn,
        );
        item(
            ui,
            ph::PENCIL_SIMPLE,
            "Rename symbol",
            "Ctrl+R",
            EditorAction::Rename,
        );
        item(
            ui,
            ph::ARROW_SQUARE_OUT,
            "Go to definition",
            "F12",
            EditorAction::GoToDef,
        );
        item(
            ui,
            ph::PUZZLE_PIECE,
            "Go to implementation",
            "Ctrl+F12",
            EditorAction::GoToImpl,
        );
        item(
            ui,
            ph::EYE,
            "Add to Watch (Debug)",
            "",
            EditorAction::AddWatch,
        );
        item(
            ui,
            ph::TIMER,
            "Time this function (Profile)",
            "",
            EditorAction::TimeFunction,
        );
        item(
            ui,
            ph::TIMER,
            "Time selected lines (Profile)",
            "",
            EditorAction::TimeLines,
        );
    }
    if is_rs || is_cargo {
        ui.separator();
        let label = if is_cargo {
            "Dependency completion"
        } else {
            "Code suggestions"
        };
        item(
            ui,
            ph::LIST_BULLETS,
            label,
            "Ctrl+Space",
            EditorAction::Completion,
        );
    }

    ui.separator();
    item(ui, ph::COPY, "Copy", "Ctrl+C", EditorAction::Copy);
    item(
        ui,
        ph::BRACKETS_CURLY,
        "Select & copy block",
        "Ctrl+[",
        EditorAction::SelectBlock,
    );
    item(
        ui,
        ph::SELECTION_ALL,
        "Select all",
        "Ctrl+A",
        EditorAction::SelectAll,
    );

    ui.separator();
    item(
        ui,
        ph::MAGNIFYING_GLASS_PLUS,
        "Zoom in",
        "Ctrl++",
        EditorAction::ZoomIn,
    );
    item(
        ui,
        ph::MAGNIFYING_GLASS_MINUS,
        "Zoom out",
        "Ctrl+-",
        EditorAction::ZoomOut,
    );
    item(
        ui,
        ph::ARROW_COUNTER_CLOCKWISE,
        "Reset zoom",
        "Ctrl+0",
        EditorAction::ZoomReset,
    );

    chosen
}
