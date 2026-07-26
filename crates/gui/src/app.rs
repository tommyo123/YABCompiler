//! egui front-end for YABCompiler.
//!
//! All UI state lives on `YabApp`. The actual compilation pipeline
//! and PRG handling live in [`pipeline`]; this file is allowed to
//! know about widgets and paths but not about IR/codegen types.

use std::path::PathBuf;

use eframe::egui;
use egui::{Color32, RichText};
use yabcompiler_core::config;

use crate::pipeline::{
    self, BuildArtifact, BuildRequest, Profile, default_asm_for, default_output_for,
};
use crate::theme::{self, Flavor};

/// Three-way profile pick mirrored as a UI enum so we can drive
/// `egui::SelectableLabel` directly. Translated to `Profile` in
/// `pipeline` calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileChoice {
    #[default]
    Default,
    Speed,
    Size,
}

impl ProfileChoice {
    pub fn as_profile(self) -> Profile {
        match self {
            Self::Default => Profile::Default,
            Self::Speed => Profile::Speed,
            Self::Size => Profile::Size,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Speed => "Speed",
            Self::Size => "Size",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Default => "win-win optimisations only (smaller AND faster)",
            Self::Speed => "allow size-growing speedups (inlining, unrolling)",
            Self::Size => "allow speed-losing shrinks (shared helpers)",
        }
    }
}

/// What the bottom `Output` panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum OutputView {
    #[default]
    Log,
    Asm,
}

pub struct YabApp {
    // Files
    input_path: PathBuf,
    output_path: PathBuf,
    asm_path: PathBuf,
    write_asm: bool,

    // Compile options
    profile: ProfileChoice,
    extraram: bool,
    force_extraram_off: bool,
    auto_reserve: bool,
    lenient_syntax: bool,
    safe_sys_calls: bool,
    rem_hint_dialect: yabcompiler_core::BasicHintDialect,
    reserved_ranges: String,
    custom_start_enabled: bool,
    custom_start_address: String,

    // Output
    log: String,
    last_asm: String,
    output_view: OutputView,
    last_status: Status,
    /// Last successful build's diagnostics: layout, extraram, auto-
    /// reserved ranges. Surfaces in the side panel and feeds the
    /// "use auto ranges as manual" shortcut. Cleared whenever the
    /// input path changes so a stale set from the previous program
    /// doesn't leak into the next compile.
    last_diagnostics: Option<yabcompiler_core::Diagnostics>,
    last_diagnostics_input: PathBuf,

    // Theme
    flavor: Flavor,
    theme_initialised: bool,

    // Menu / dialogs
    about_open: bool,
}

impl Default for YabApp {
    fn default() -> Self {
        Self {
            input_path: PathBuf::new(),
            output_path: PathBuf::new(),
            asm_path: PathBuf::new(),
            write_asm: false,
            profile: ProfileChoice::default(),
            extraram: false,
            force_extraram_off: false,
            auto_reserve: true,
            lenient_syntax: false,
            safe_sys_calls: false,
            rem_hint_dialect: yabcompiler_core::BasicHintDialect::None,
            reserved_ranges: String::new(),
            custom_start_enabled: false,
            custom_start_address: "$C000".to_string(),
            log: String::new(),
            last_asm: String::new(),
            output_view: OutputView::default(),
            last_status: Status::default(),
            last_diagnostics: None,
            last_diagnostics_input: PathBuf::new(),
            flavor: Flavor::default(),
            theme_initialised: false,
            about_open: false,
        }
    }
}

#[derive(Default, Clone)]
enum Status {
    #[default]
    Idle,
    Ok(String),
    Err(String),
}

impl YabApp {
    /// Build the app, restoring the previously-chosen theme from
    /// eframe's cross-platform storage (the OS config dir on Windows,
    /// macOS and Linux). Falls back to Latte on first run or if the
    /// stored value is missing/unrecognised.
    pub fn new(storage: Option<&dyn eframe::Storage>) -> Self {
        let flavor = storage
            .and_then(|s| s.get_string("theme"))
            .and_then(|key| Flavor::from_key(&key))
            .unwrap_or(Flavor::Latte);
        Self {
            flavor,
            ..Self::default()
        }
    }

    /// The currently-selected theme flavor (used by `main` to apply
    /// the restored theme before the first frame).
    pub fn flavor(&self) -> Flavor {
        self.flavor
    }
}

impl eframe::App for YabApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Apply the theme once on first frame, and re-apply whenever
        // the user picks a different flavor. egui keeps Visuals on
        // the Style, so updating ctx.set_visuals re-themes the whole
        // tree immediately.
        if !self.theme_initialised {
            ui.ctx().set_visuals(theme::visuals(self.flavor));
            self.theme_initialised = true;
        }

        self.menu_bar(ui);
        self.top_bar(ui);
        self.settings_panel(ui);
        self.bottom_panel(ui);
        self.central_panel(ui);
        self.about_window(ui);
    }

    /// Persist the chosen theme. eframe calls this periodically and on
    /// exit, writing to the platform-standard config location.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("theme", self.flavor.key().to_string());
    }
}

impl YabApp {
    /// The application menu bar. Mirrors the toolbar actions and is the
    /// sole home for theme selection and the Exit / About entries.
    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu-bar").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let can_run = !self.input_path.as_os_str().is_empty();

                ui.menu_button("File", |ui| {
                    if ui.button("Open input…").clicked() {
                        self.browse_input();
                        ui.close();
                    }
                    ui.add_enabled_ui(can_run, |ui| {
                        if ui.button("Compile").clicked() {
                            self.run_compile();
                            ui.close();
                        }
                    });
                    ui.separator();
                    ui.add_enabled_ui(can_run, |ui| {
                        if ui.button("Export as BASIC (.bas)…").clicked() {
                            self.run_export_bas();
                            ui.close();
                        }
                        if ui.button("Export as PRG (.prg)…").clicked() {
                            self.run_export_prg();
                            ui.close();
                        }
                    });
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("Tools", |ui| {
                    ui.add_enabled_ui(can_run, |ui| {
                        if ui.button("List (detokenise)").clicked() {
                            self.run_list();
                            ui.close();
                        }
                        if ui.button("Dump (tokens + bytes)").clicked() {
                            self.run_dump();
                            ui.close();
                        }
                    });
                    ui.separator();
                    if ui.button("Clear output").clicked() {
                        self.log.clear();
                        ui.close();
                    }
                });

                ui.menu_button("View", |ui| {
                    ui.menu_button("Theme", |ui| {
                        for flavor in [
                            Flavor::Latte,
                            Flavor::Frappe,
                            Flavor::Macchiato,
                            Flavor::Mocha,
                        ] {
                            if ui
                                .selectable_label(self.flavor == flavor, flavor.label())
                                .clicked()
                            {
                                self.flavor = flavor;
                                ui.ctx().set_visuals(theme::visuals(flavor));
                                ui.close();
                            }
                        }
                    });
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("About YABCompiler…").clicked() {
                        self.about_open = true;
                        ui.close();
                    }
                });
            });
        });
    }

    /// Modal-ish "About" window, toggled from Help ▸ About.
    fn about_window(&mut self, ui: &mut egui::Ui) {
        egui::Window::new("About YABCompiler")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut self.about_open)
            .show(ui.ctx(), |ui| {
                ui.add_space(4.0);
                ui.heading(RichText::new(config::APP_NAME).strong());
                ui.label(RichText::new(config::TAGLINE).color(theme_subtle_color(ui)));
                ui.add_space(8.0);
                ui.label(format!("Version {}", config::VERSION));
                ui.label(format!("by {}", config::AUTHOR));
                ui.add_space(8.0);
                ui.label("Compiles Commodore BASIC V2 (plus a Simons' BASIC subset)");
                ui.label("to native 6502 machine code that runs on a stock C64.");
                ui.add_space(8.0);
                ui.hyperlink(config::HOMEPAGE);
                ui.add_space(4.0);
                ui.label(
                    RichText::new("MIT licensed")
                        .color(theme_subtle_color(ui))
                        .small(),
                );
                ui.add_space(8.0);
            });
    }

    /// Open a file picker for the input program and adopt the choice.
    /// Shared by the Files panel button and File ▸ Open.
    fn browse_input(&mut self) {
        if let Some(picked) = rfd::FileDialog::new()
            .add_filter("BASIC source or PRG", &["bas", "prg"])
            .add_filter("BASIC source", &["bas"])
            .add_filter("Commodore PRG", &["prg"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            self.adopt_input(picked);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top-bar")
            .exact_size(48.0)
            .show_inside(ui, |ui| {
                ui.add_space(8.0);
                // The product name now lives in the OS title bar, so the
                // toolbar is just the action buttons — centered rather
                // than hugging the left edge.
                ui.vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        let can_run = !self.input_path.as_os_str().is_empty();
                        ui.add_enabled_ui(can_run, |ui| {
                            if ui
                                .button(RichText::new("Compile").strong())
                                .on_hover_text("Run the full pipeline and write a runnable .prg")
                                .clicked()
                            {
                                self.run_compile();
                            }
                            if ui
                                .button("List")
                                .on_hover_text("Detokenise the input as readable BASIC")
                                .clicked()
                            {
                                self.run_list();
                            }
                            if ui
                                .button("Dump")
                                .on_hover_text("Show parsed lines + raw token bytes")
                                .clicked()
                            {
                                self.run_dump();
                            }
                            ui.separator();
                            if ui
                                .button("Export .bas")
                                .on_hover_text(
                                    "Save the input program as detokenised BASIC source.\n\
                                 Auto-detects whether the input is .bas (re-tokenises\n\
                                 first to canonicalise) or .prg (detokenises directly).\n\
                                 Handles BASIC v2 keywords and extended tokens.",
                                )
                                .clicked()
                            {
                                self.run_export_bas();
                            }
                            if ui
                                .button("Export .prg")
                                .on_hover_text(
                                    "Save the input program as a tokenised .prg.\n\
                                 Auto-detects whether the input is .bas (tokenises) or\n\
                                 .prg (passes through after a sanity round-trip).\n\
                                 Handles BASIC v2 keywords and extended tokens.",
                                )
                                .clicked()
                            {
                                self.run_export_prg();
                            }
                        });
                    });
                });
            });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("settings")
            .resizable(true)
            .default_size(360.0)
            .min_size(320.0)
            .show_inside(ui, |ui| {
                ui.add_space(8.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.files_section(ui);
                    ui.add_space(12.0);
                    self.profile_section(ui);
                    ui.add_space(12.0);
                    self.memory_section(ui);
                    ui.add_space(12.0);
                    self.reserved_section(ui);
                });
            });
    }

    fn files_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Files");
        ui.add_space(4.0);

        // Input file
        ui.label(RichText::new("Input source").strong());
        ui.horizontal(|ui| {
            let mut text = path_string(&self.input_path);
            if ui
                .add(
                    egui::TextEdit::singleline(&mut text)
                        .hint_text("path to BASIC source (.bas) or tokenized .prg")
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                self.adopt_input(PathBuf::from(text));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("📂 Browse…").clicked() {
                self.browse_input();
            }
            if !self.input_path.as_os_str().is_empty() {
                ui.label(
                    RichText::new(short_path(&self.input_path))
                        .color(theme_subtle_color(ui))
                        .small(),
                );
            }
        });

        ui.add_space(8.0);

        // Output file
        ui.label(RichText::new("Output .prg").strong());
        ui.horizontal(|ui| {
            let mut text = path_string(&self.output_path);
            if ui
                .add(
                    egui::TextEdit::singleline(&mut text)
                        .hint_text("output path (auto-derived from input)")
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                self.output_path = PathBuf::from(text);
            }
        });
        if ui.button("📂 Browse…").clicked() {
            let mut dialog = rfd::FileDialog::new().add_filter("Commodore PRG", &["prg"]);
            if let Some(parent) = self
                .output_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                dialog = dialog.set_directory(parent);
            }
            if let Some(name) = self.output_path.file_name().and_then(|n| n.to_str()) {
                dialog = dialog.set_file_name(name);
            }
            if let Some(picked) = dialog.save_file() {
                self.output_path = picked;
            }
        }

        ui.add_space(8.0);

        // ASM file (optional)
        ui.checkbox(&mut self.write_asm, "Write assembly listing")
            .on_hover_text("Save the generated 6502 assembly alongside the .prg");
        ui.add_enabled_ui(self.write_asm, |ui| {
            ui.horizontal(|ui| {
                let mut text = path_string(&self.asm_path);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut text)
                            .hint_text("output .s path")
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    self.asm_path = PathBuf::from(text);
                }
            });
            if ui.button("📂 Browse…").clicked() {
                if let Some(picked) = rfd::FileDialog::new()
                    .add_filter("Assembly", &["s", "asm"])
                    .add_filter("All files", &["*"])
                    .save_file()
                {
                    self.asm_path = picked;
                }
            }
        });
    }

    fn profile_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Optimisation profile");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for choice in [
                ProfileChoice::Default,
                ProfileChoice::Speed,
                ProfileChoice::Size,
            ] {
                let selected = self.profile == choice;
                if ui
                    .add(egui::Button::selectable(selected, choice.label()))
                    .on_hover_text(choice.description())
                    .clicked()
                {
                    self.profile = choice;
                }
            }
        });
        ui.label(
            RichText::new(self.profile.description())
                .color(theme_subtle_color(ui))
                .small(),
        );
    }

    fn memory_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Memory layout");
        ui.add_space(4.0);

        let extraram_disabled_by_force = self.force_extraram_off;
        ui.add_enabled_ui(!extraram_disabled_by_force, |ui| {
            ui.checkbox(&mut self.extraram, "Force extraram on")
                .on_hover_text(
                    "Bank BASIC ROM out so $A000–$BFFF becomes RAM for code/data.\n\
                    ROM calls are routed through a low jump-table that flips $01.",
                );
        });

        let force_off_disabled_by_extraram = self.extraram;
        ui.add_enabled_ui(!force_off_disabled_by_extraram, |ui| {
            ui.checkbox(&mut self.force_extraram_off, "Force extraram off")
                .on_hover_text(
                    "Disable the auto-predictor and never bank ROM out.\n\
                    Use when ROM access is required (e.g. mixed compiled/interpreted code).",
                );
        });

        if self.extraram && self.force_extraram_off {
            ui.colored_label(
                error_color(ui),
                "extraram and force-off are mutually exclusive",
            );
        } else if !self.extraram && !self.force_extraram_off && !self.custom_start_enabled {
            ui.label(
                RichText::new("auto: predictor switches extraram on near $9F00")
                    .color(theme_subtle_color(ui))
                    .small(),
            );
        }

        ui.add_space(8.0);
        ui.checkbox(&mut self.custom_start_enabled, "Custom start address")
            .on_hover_text(
                "Skip the SYS launcher. The .prg loads at the address below\n\
                 and you start it manually from BASIC (`SYS <decimal>`).\n\
                 Default ($0801 + SYS 2061) is used when unchecked.",
            );
        ui.add_enabled_ui(self.custom_start_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Address:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_start_address)
                        .hint_text("$C000")
                        .desired_width(120.0)
                        .font(egui::TextStyle::Monospace),
                );
            });
            let trimmed = self.custom_start_address.trim();
            if trimmed.is_empty() {
                ui.colored_label(error_color(ui), "address required");
            } else {
                match yabcompiler_core::parse_start_address(trimmed) {
                    Ok(addr) => ui.label(
                        RichText::new(format!("✓ load at ${addr:04X}, run with `SYS {addr}`"))
                            .color(ok_color(ui))
                            .small(),
                    ),
                    Err(e) => ui.colored_label(error_color(ui), e),
                };
            }
        });
    }

    fn reserved_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Reserved memory");
        ui.add_space(4.0);
        ui.checkbox(&mut self.auto_reserve, "Auto-reserve POKE/PEEK targets")
            .on_hover_text(
                "Scan the optimised IR for literal POKE/PEEK addresses and reserve\n\
                 those regions so generated code/data avoids sprite blocks, screens,\n\
                 character sets, etc.",
            );
        ui.checkbox(&mut self.lenient_syntax, "Ignore syntax errors")
            .on_hover_text(
                "Accept BASIC v2 typos that the interpreter only catches at runtime\n\
                 (e.g. `GOT1200` instead of `GOTO1200`, `CLOSE 4,4`). Use when\n\
                 you know the offending line is dead code.",
            );
        ui.checkbox(&mut self.safe_sys_calls, "Safe SYS calls")
            .on_hover_text(
                "Save/restore $FB-$FE and every ZP-pool cell the compiler\n\
                 allocated around each SYS. Turn on when calling third-party\n\
                 ML routines that may clobber zero page.",
            );

        ui.add_space(4.0);
        ui.label(RichText::new("REM-hint dialect").strong());
        let current = self.rem_hint_dialect;
        let label = match current {
            yabcompiler_core::BasicHintDialect::None => "None",
            yabcompiler_core::BasicHintDialect::Basic64 => "Basic 64",
            yabcompiler_core::BasicHintDialect::BasicBoss => "Basic-Boss",
        };
        egui::ComboBox::from_id_salt("rem_hint_dialect")
            .selected_text(label)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.rem_hint_dialect,
                    yabcompiler_core::BasicHintDialect::None,
                    "None",
                );
                ui.selectable_value(
                    &mut self.rem_hint_dialect,
                    yabcompiler_core::BasicHintDialect::Basic64,
                    "Basic 64",
                )
                .on_hover_text("Honour `REM@i=...` (integer) and `REM@b=...` (byte).");
                ui.selectable_value(
                    &mut self.rem_hint_dialect,
                    yabcompiler_core::BasicHintDialect::BasicBoss,
                    "Basic-Boss",
                )
                .on_hover_text(
                    "Honour `REM@ \\BYTE ...`, `\\WORD ...`, and the `=FAST` zero-page suffix.",
                );
            });

        ui.add_space(6.0);
        ui.label(RichText::new("Manual ranges").strong());
        ui.label(
            RichText::new("comma-separated, e.g. $7800-$79FF, $C000")
                .color(theme_subtle_color(ui))
                .small(),
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.reserved_ranges)
                .hint_text("$7800-$79FF, $C000")
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );
        let trimmed = self.reserved_ranges.trim();
        if !trimmed.is_empty() {
            match yabcompiler_core::parse_reserved_ranges(trimmed) {
                Ok(ranges) => {
                    let s = ranges
                        .iter()
                        .map(|(s, e)| {
                            if s == e {
                                format!("${s:04X}")
                            } else {
                                format!("${s:04X}–${e:04X}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    ui.label(
                        RichText::new(format!("✓ {} range(s): {s}", ranges.len()))
                            .color(ok_color(ui))
                            .small(),
                    );
                }
                Err(e) => {
                    ui.label(
                        RichText::new(format!("✗ {e}"))
                            .color(error_color(ui))
                            .small(),
                    );
                }
            }
        }

        // Show the auto-discovered ranges from the most recent
        // successful compile of THIS program. Lets the user audit
        // what auto-reserve picked, and (if they want manual
        // control next time) prefill the manual field with the
        // exact same set so a re-compile is reproducible without
        // hand-typing.
        if let Some(d) = self
            .last_diagnostics
            .as_ref()
            .filter(|_| self.last_diagnostics_input == self.input_path)
            && !d.auto_reserved.is_empty()
        {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Auto-discovered:").strong().small());
                ui.label(
                    RichText::new(format_ranges(&d.auto_reserved))
                        .color(theme_subtle_color(ui))
                        .small()
                        .monospace(),
                );
            });
            let formatted = format_ranges(&d.auto_reserved);
            if ui
                .small_button("Copy to manual")
                .on_hover_text(
                    "Replace the manual ranges field with this exact set.\n\
                     Useful when you want to pin a layout that auto-reserve\n\
                     normally rediscovers each compile.",
                )
                .clicked()
            {
                self.reserved_ranges = formatted;
            }
        }
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status")
            .exact_size(28.0)
            .show_inside(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    match &self.last_status {
                        Status::Idle => {
                            ui.label(RichText::new("Ready").color(theme_subtle_color(ui)).small());
                        }
                        Status::Ok(msg) => {
                            ui.label(RichText::new(format!("✓ {msg}")).color(ok_color(ui)));
                        }
                        Status::Err(msg) => {
                            ui.label(RichText::new(format!("✗ {msg}")).color(error_color(ui)));
                        }
                    }
                });
            });
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Output");
                ui.add_space(12.0);
                ui.selectable_value(&mut self.output_view, OutputView::Log, "Log / listing");
                ui.add_enabled_ui(!self.last_asm.is_empty(), |ui| {
                    ui.selectable_value(&mut self.output_view, OutputView::Asm, "Generated asm");
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Clear").clicked() {
                        self.log.clear();
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let body = match self.output_view {
                        OutputView::Log => &mut self.log,
                        OutputView::Asm => &mut self.last_asm,
                    };
                    ui.add(
                        egui::TextEdit::multiline(body)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(24),
                    );
                });
        });
    }
}

// Action handlers --------------------------------------------------

impl YabApp {
    fn adopt_input(&mut self, picked: PathBuf) {
        // Re-derive output and asm paths from the new input every time
        // the user picks a file — otherwise switching to a different
        // source while the prior output path is still in the field
        // silently overwrites the wrong target on the next Compile.
        self.output_path = default_output_for(&picked);
        self.asm_path = default_asm_for(&picked);
        if self.last_diagnostics_input != picked {
            // Different program: every per-file setting from the
            // prior compile is suspect — the previous program's
            // reserved ranges / extraram choice / custom origin /
            // profile have no reason to carry over to a fresh file.
            // Snap everything back to defaults so the new build
            // starts from a known baseline (the user's theme stays
            // since that's a session preference, not a program one).
            self.profile = ProfileChoice::default();
            self.extraram = false;
            self.force_extraram_off = false;
            self.auto_reserve = true;
            self.lenient_syntax = false;
            self.safe_sys_calls = false;
            self.rem_hint_dialect = yabcompiler_core::BasicHintDialect::None;
            self.reserved_ranges.clear();
            self.custom_start_enabled = false;
            self.custom_start_address = "$C000".to_string();
            self.write_asm = false;
            self.last_diagnostics = None;
            self.last_asm.clear();
            self.log.clear();
            self.last_status = Status::default();
            self.output_view = OutputView::default();
        }
        self.input_path = picked;
    }

    fn run_list(&mut self) {
        match pipeline::list_program(&self.input_path) {
            Ok(text) => {
                self.log = text;
                self.output_view = OutputView::Log;
                self.last_status = Status::Ok("listed".to_string());
            }
            Err(e) => {
                self.log = format!("error: {e}\n");
                self.last_status = Status::Err(e);
            }
        }
    }

    fn run_dump(&mut self) {
        match pipeline::dump_program(&self.input_path) {
            Ok(text) => {
                self.log = text;
                self.output_view = OutputView::Log;
                self.last_status = Status::Ok("dumped".to_string());
            }
            Err(e) => {
                self.log = format!("error: {e}\n");
                self.last_status = Status::Err(e);
            }
        }
    }

    fn run_export_bas(&mut self) {
        let default_name = pipeline::default_export_path(&self.input_path, "bas");
        let parent = default_name
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf());
        let mut dialog = rfd::FileDialog::new()
            .add_filter("BASIC source", &["bas"])
            .add_filter("All files", &["*"]);
        if let Some(p) = parent {
            dialog = dialog.set_directory(p);
        }
        if let Some(name) = default_name.file_name().and_then(|n| n.to_str()) {
            dialog = dialog.set_file_name(name);
        }
        let Some(target) = dialog.save_file() else {
            return;
        };
        match pipeline::export_as_bas(&self.input_path, &target) {
            Ok(bytes_written) => {
                let summary = format!(
                    "exported BASIC source: {} ({bytes_written} bytes)",
                    target.display()
                );
                self.log = format!("{summary}\n");
                self.last_status = Status::Ok(summary);
            }
            Err(e) => {
                self.log = format!("error: {e}\n");
                self.last_status = Status::Err(e);
            }
        }
    }

    fn run_export_prg(&mut self) {
        let default_name = pipeline::default_export_path(&self.input_path, "prg");
        let parent = default_name
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf());
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Commodore PRG", &["prg"])
            .add_filter("All files", &["*"]);
        if let Some(p) = parent {
            dialog = dialog.set_directory(p);
        }
        if let Some(name) = default_name.file_name().and_then(|n| n.to_str()) {
            dialog = dialog.set_file_name(name);
        }
        let Some(target) = dialog.save_file() else {
            return;
        };
        match pipeline::export_as_prg(&self.input_path, &target) {
            Ok(bytes_written) => {
                let summary = format!(
                    "exported tokenised .prg: {} ({bytes_written} bytes)",
                    target.display()
                );
                self.log = format!("{summary}\n");
                self.last_status = Status::Ok(summary);
            }
            Err(e) => {
                self.log = format!("error: {e}\n");
                self.last_status = Status::Err(e);
            }
        }
    }

    fn run_compile(&mut self) {
        let custom_start = if self.custom_start_enabled {
            Some(self.custom_start_address.as_str())
        } else {
            None
        };
        let request = BuildRequest {
            input_path: &self.input_path,
            profile: self.profile.as_profile(),
            extraram: self.extraram,
            force_extraram_off: self.force_extraram_off,
            auto_reserve: self.auto_reserve,
            lenient_syntax: self.lenient_syntax,
            safe_sys_calls: self.safe_sys_calls,
            rem_hint_dialect: self.rem_hint_dialect,
            reserved_text: &self.reserved_ranges,
            custom_start_text: custom_start,
        };

        match pipeline::build(&request) {
            Ok(artifact) => self.handle_compile_ok(artifact),
            Err(e) => self.handle_compile_err(e),
        }
    }

    fn handle_compile_err(&mut self, e: pipeline::BuildError) {
        // Reserve the status bar for the short message; surface the
        // assembly (when there is one) in the log so the user can
        // copy it out without parsing the diagnostic text.
        let mut log = format!("error: {}\n", e.message);
        if let Some(asm) = e.asm.as_ref() {
            self.last_asm = asm.clone();
            log.push_str("\n(generated assembly available in the Assembly tab)\n");
            self.output_view = OutputView::Asm;
        }
        self.log = log;
        self.last_status = Status::Err(e.message);
        // A failed compile invalidates any prior diagnostics — clear
        // them so the side panel doesn't keep showing layout from a
        // previous successful build.
        self.last_diagnostics = None;
    }

    fn handle_compile_ok(&mut self, artifact: BuildArtifact) {
        let out_path = if self.output_path.as_os_str().is_empty() {
            default_output_for(&self.input_path)
        } else {
            self.output_path.clone()
        };

        if let Err(e) = std::fs::write(&out_path, &artifact.prg_bytes) {
            let msg = format!("write {}: {e}", out_path.display());
            self.log = format!("error: {msg}\n");
            self.last_status = Status::Err(msg);
            return;
        }

        if self.write_asm {
            let asm_path = if self.asm_path.as_os_str().is_empty() {
                default_asm_for(&self.input_path)
            } else {
                self.asm_path.clone()
            };
            if let Err(e) = std::fs::write(&asm_path, &artifact.asm) {
                let msg = format!("write {}: {e}", asm_path.display());
                self.log = format!("error: {msg}\n");
                self.last_status = Status::Err(msg);
                return;
            }
        }

        let summary = format!(
            "compiled {} ({} bytes machine code, {} bytes total .prg)",
            out_path.display(),
            artifact.machine_code_len,
            artifact.prg_bytes.len(),
        );
        let diagnostics_text = format_diagnostics(&artifact.diagnostics);
        self.log = format!("{summary}\n{diagnostics_text}");
        self.last_asm = artifact.asm;
        self.output_view = OutputView::Log;
        self.last_status = Status::Ok(summary);
        self.last_diagnostics = Some(artifact.diagnostics);
        self.last_diagnostics_input = self.input_path.clone();
    }
}

fn format_diagnostics(d: &yabcompiler_core::Diagnostics) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  layout : load ${:04X}, code ends ${:04X}\n",
        d.start_address, d.end_address
    ));
    out.push_str(&format!("  extraram: {}\n", d.extraram));
    if !d.effective_reserved.is_empty() {
        let source = match (d.auto_reserved.is_empty(), d.manual_reserved.is_empty()) {
            (true, true) => "",
            (false, true) => " (auto)",
            (true, false) => " (manual)",
            (false, false) => " (auto + manual)",
        };
        out.push_str(&format!(
            "  reserved{source}: {}\n",
            format_ranges(&d.effective_reserved)
        ));
    }
    for s in &d.skipped_statements {
        let scope = if s.whole_conditional {
            "conditional dropped"
        } else {
            "statement dropped"
        };
        out.push_str(&format!(
            "  warning: line {}: token ${:02X} is not supported, {scope}\n",
            s.line, s.token
        ));
    }
    out
}

fn format_ranges(ranges: &[(u16, u16)]) -> String {
    ranges
        .iter()
        .map(|(s, e)| {
            if s == e {
                format!("${s:04X}")
            } else {
                format!("${s:04X}-${e:04X}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// Helpers ----------------------------------------------------------

fn path_string(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

fn short_path(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn theme_subtle_color(ui: &egui::Ui) -> Color32 {
    ui.visuals().widgets.noninteractive.fg_stroke.color
}

fn ok_color(ui: &egui::Ui) -> Color32 {
    let v = ui.visuals();
    if v.dark_mode {
        Color32::from_rgb(0xa6, 0xe3, 0xa1) // mocha green-ish
    } else {
        Color32::from_rgb(0x40, 0xa0, 0x2b) // latte green
    }
}

fn error_color(ui: &egui::Ui) -> Color32 {
    ui.visuals().error_fg_color
}
