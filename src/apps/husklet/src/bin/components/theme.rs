const BG0: &str = "#0d0e11"; // window ground
const BG1: &str = "#15171c"; // strips / sidebars
const BG2: &str = "#1a1d23"; // cards / terminal
const BG3: &str = "#232732"; // hover / raised
const LINE: &str = "#2b2f39";
const LINE_S: &str = "#20232b";
const CONTROL_LINE: &str = "#606878";
const TXT: &str = "#e7e9ee";
const DIM: &str = "#878e9c";
const FAINT: &str = "#818896";
pub(crate) const ACCENT: &str = "#2f80ff";
const ACCENT_FILL: &str = "#2a6eca";
const ACCENT_FILL_HOVER: &str = "#3275d4";

pub(crate) fn css() -> String {
    format!(
	"
	window {{ background-color:{BG0}; color:{TXT}; }}
	label {{ color:{TXT}; }}

/* ---- generic slim controls ---- */
.strip {{ background-color:{BG1}; box-shadow: inset 0 -1px 0 0 {LINE_S}; min-height:38px; padding:0 10px 0 14px; }}
.h {{ font-size:14px; font-weight:700; letter-spacing:-.01em; }}
/* Unified button — used by New, Launch, Cancel, Create, Browse. */
	.tbtn, .btn {{ font-size:12.5px; font-weight:600; color:{TXT}; background-color:{BG2}; border:1px solid {CONTROL_LINE}; border-radius:7px; padding:5px 13px; min-height:0; box-shadow:none; }}
.tbtn:hover, .btn:hover {{ background-color:{BG3}; border-color:{DIM}; }}
.tbtn .plus {{ color:{ACCENT}; font-weight:700; }}

/* ---- manager list ---- */
list.wslist {{ background:transparent; padding:6px 8px; }}
list.wslist > row {{ background:transparent; border-radius:9px; margin:2px 4px; padding:0; }}
list.wslist > row:hover {{ background-color:{BG1}; }}
list.wslist > row:selected {{ background-color:{BG1}; }}
.wsrow {{ padding:9px 11px; }}
.wsrow .nm {{ font-size:13.5px; font-weight:600; letter-spacing:-.01em; }}
.wsrow .mt {{ font-size:11.5px; color:{DIM}; font-family:'SF Mono',ui-monospace,monospace; }}
.chip {{ font-family:'SF Mono',ui-monospace,monospace; font-size:10.5px; font-weight:600; padding:2px 6px; border-radius:5px; }}
.chip.arm {{ color:#2dd4bf; background:rgba(45,212,191,.15); }}
.chip.amd {{ color:#a78bfa; background:rgba(167,139,250,.16); }}
.chip.dar {{ color:#f0a35e; background:rgba(240,163,94,.15); }}
.go {{ font-size:12px; color:{ACCENT}; font-weight:600; }}
.empty {{ color:{DIM}; font-size:13px; padding:26px; }}
/* per-row action affordances: ▶ play + ⋯ menu — frameless (no button box), color-only hover */
	.rowbtn, .rowbtn > button {{ min-height:0; min-width:0; padding:2px 7px; background:none; border:none; box-shadow:none; color:{DIM}; }}
.rowbtn:hover, .rowbtn:hover > button, .rowbtn > button:hover {{ color:{TXT}; background:none; }}
.rowbtn > button:checked, .rowbtn > button:active {{ background:none; box-shadow:none; }}
.rowbtn image {{ -gtk-icon-size:15px; }}
.rowbtn .dots {{ font-size:18px; font-weight:700; margin-top:-8px; letter-spacing:1px; }}
	.rowmenu contents {{ background-color:{BG2}; border:1px solid {CONTROL_LINE}; border-radius:9px; padding:5px; }}
.menuitem {{ background:transparent; border:none; box-shadow:none; padding:7px 14px; border-radius:6px; color:{TXT}; font-size:12.5px; }}
.menuitem:hover {{ background-color:{BG3}; }}

/* ---- new-workspace sheet ---- */
.nav {{ background-color:{BG1}; box-shadow: inset -1px 0 0 0 {LINE_S}; padding:10px 8px; min-width:150px; }}
.navi {{ padding:7px 10px; border-radius:7px; color:{DIM}; font-weight:500; font-size:12.5px; }}
.navi:hover {{ background-color:{BG3}; color:{TXT}; }}
.navi.on {{ background-color:{BG3}; color:{TXT}; }}
.pane {{ padding:18px 20px; }}
.ptitle {{ font-size:13px; font-weight:650; }}
.flabel {{ font-size:11px; color:{DIM}; font-weight:600; }}
.fhint {{ font-size:11px; color:{FAINT}; }}
	entry {{ background-color:{BG2}; color:{TXT}; border:1px solid {CONTROL_LINE}; border-radius:7px; padding:6px 9px; min-height:0; caret-color:{ACCENT}; }}
entry:focus {{ border-color:{ACCENT}; }}
entry.mono {{ font-family:'SF Mono',ui-monospace,monospace; font-size:12.5px; }}
	spinbutton {{ background-color:{BG2}; border:1px solid {CONTROL_LINE}; border-radius:7px; color:{TXT}; min-height:0; }}
spinbutton entry {{ border:none; background:transparent; }}
	.seg {{ background-color:{BG2}; border:1px solid {CONTROL_LINE}; border-radius:7px; padding:2px; }}
.seg button {{ font-family:'SF Mono',ui-monospace,monospace; font-size:11.5px; color:{DIM}; background:transparent; border:none; border-radius:5px; padding:4px 12px; min-height:0; box-shadow:none; }}
	.seg button:checked {{ background-color:{ACCENT_FILL}; color:#fff; font-weight:600; }}
.xbtn {{ color:{DIM}; background:transparent; border:1px solid transparent; border-radius:7px; min-height:0; min-width:32px; padding:5px; }}
.xbtn:hover {{ color:#ff6b6b; background-color:rgba(255,90,90,.12); }}
.xbtn image {{ -gtk-icon-size:15px; }}
/* macOS-like slim toggle */
.dockrow switch {{ min-width:38px; min-height:21px; border-radius:11px; }}
.dockrow switch > slider {{ min-width:17px; min-height:17px; margin:1px; border-radius:50%; }}
/* required-field error state */
entry.err {{ border-color:#ff6b6b; box-shadow:0 0 0 2px rgba(255,90,90,.22); }}
.addrow {{ font-size:11.5px; color:{ACCENT}; font-weight:600; background:transparent; border:none; box-shadow:none; padding:2px 0; min-height:0; }}
.footer {{ background-color:{BG1}; box-shadow: inset 0 1px 0 0 {LINE_S}; padding:10px 14px 16px; }}
	.btn.primary {{ background-color:{ACCENT_FILL}; border-color:{ACCENT_FILL}; color:#fff; }}
	.btn.primary:hover {{ background-color:{ACCENT_FILL_HOVER}; }}
	.dockrow {{ background-color:{BG2}; border:1px solid {CONTROL_LINE}; border-radius:8px; padding:10px 12px; }}
.dockrow .tt {{ font-size:12.5px; font-weight:600; }}
.dockrow .td {{ font-size:11px; color:{DIM}; }}
/* image-selection window */
.imghead {{ padding:16px 18px 8px 18px; }}
.imglist {{ background:transparent; padding:4px 10px; }}
.imglist > row {{ border-radius:8px; margin:2px 0; }}
.imglist > row:hover {{ background-color:{BG3}; }}
.imgrow {{ padding:9px 12px; }}
.imgname {{ font-size:13px; font-weight:600; color:{TXT}; }}
.imgref {{ font-size:11px; color:{DIM}; font-family:'SF Mono',ui-monospace,monospace; }}

/* ---- terminal window ---- */
.tabbar {{ background-color:{BG1}; box-shadow: inset 0 -1px 0 0 {LINE_S}; min-height:34px; }}
.tab {{ background-color:{BG1}; color:{DIM}; box-shadow: inset -1px 0 0 0 {LINE_S}; padding:0 10px; }}
.tab:hover {{ background-color:{BG3}; color:{TXT}; }}
.tab.on {{ background-color:{BG2}; color:{TXT}; box-shadow: inset -1px 0 0 0 {LINE_S}, inset 0 -2px 0 0 {ACCENT}; }}
.tab label {{ font-size:12px; font-weight:500; }}
.tab .di {{ color:{ACCENT}; }}
button.tabx {{ min-height:16px; min-width:16px; padding:0; margin-left:6px; background:transparent; border:none; box-shadow:none; opacity:0; color:{DIM}; }}
.tab:hover button.tabx, .tab.on button.tabx {{ opacity:.6; }}
button.tabx:hover {{ opacity:1; background-color:rgba(255,255,255,.14); border-radius:4px; }}
.newtab {{ min-width:30px; padding:0; color:{DIM}; background:transparent; border:none; box-shadow: inset 1px 0 0 0 {LINE_S}; border-radius:0; }}
.newtab label {{ font-size:14px; font-weight:400; }}
.newtab:hover {{ background-color:{BG3}; color:{TXT}; }}
stack.pages {{ background-color:{BG2}; }}

/* ---- overview ---- */
.dside {{ background-color:{BG1}; padding:9px 8px; min-width:130px; }}
.dsi {{ padding:7px 10px; border-radius:7px; color:{DIM}; font-weight:500; font-size:12.5px; }}
.dsi:hover {{ background-color:{BG3}; color:{TXT}; }}
.dsi.on {{ background-color:{BG3}; color:{TXT}; }}
.dbadge {{ font-family:'SF Mono',ui-monospace,monospace; font-size:10px; color:{FAINT}; }}
.dmain {{ padding:16px 18px; }}
.dashtitle {{ font-size:16px; font-weight:700; letter-spacing:-.01em; }}
.workspace-settings {{ background-color:{BG2}; }}
.settings-identity, .settings-card {{ background-color:{BG1}; border:1px solid {CONTROL_LINE}; border-radius:11px; padding:16px; }}
.settings-identity-values {{ margin-top:2px; }}
.settings-workspace-name {{ font-size:15px; font-weight:650; }}
.settings-image {{ color:{DIM}; font-family:'SF Mono',ui-monospace,monospace; font-size:11.5px; }}
.settings-card-description {{ color:{DIM}; font-size:11.5px; margin-bottom:2px; }}
.settings-group-title {{ color:{DIM}; font-size:12px; font-weight:650; letter-spacing:.03em; }}
.settings-grid > flowboxchild {{ padding:0; min-width:220px; }}
.settings-grid > flowboxchild:selected {{ background:transparent; }}
.settings-apply-note {{ color:{DIM}; background-color:rgba(91,141,239,.09); border:1px solid rgba(91,141,239,.24); border-radius:9px; padding:10px 12px; }}
.settings-apply-note image {{ color:{ACCENT}; -gtk-icon-size:15px; }}
.settings-apply-note label {{ font-size:11.5px; }}
.settings-save-row {{ background-color:{BG1}; border:1px solid {CONTROL_LINE}; border-radius:11px; padding:12px 14px; }}
.kvk {{ font-size:11.5px; color:{DIM}; font-weight:600; }}
.kvv {{ font-size:12.5px; font-family:'SF Mono',ui-monospace,monospace; color:{TXT}; }}
/* Every table row (Processes / Containers / Images / …) is the SAME height (min-height + 0 vertical
   padding), so all the overview tables line up uniformly regardless of whether a row has buttons. */
.trow {{ padding:0 8px; min-height:32px; box-shadow: inset 0 -1px 0 0 {LINE_S}; }}
.trow.thead {{ min-height:26px; box-shadow: inset 0 -1px 0 0 {LINE}; }}
.tcell {{ font-family:'SF Mono',ui-monospace,monospace; font-size:11.5px; color:{TXT}; }}
.trow.thead .tcell {{ color:{FAINT}; font-size:10.5px; font-weight:600; letter-spacing:.04em; }}
/* Compact, flat signal buttons that fit inside a row's height (so a Processes row is no taller). */
.sigbtn {{ color:{DIM}; background:transparent; border:1px solid transparent; border-radius:6px; min-height:0; min-width:26px; padding:3px; margin:0; }}
.sigbtn image {{ -gtk-icon-size:15px; }}
.sigbtn:hover {{ color:#ff6b6b; background-color:rgba(255,90,90,.12); }}
.dhead {{ font-size:11px; font-weight:650; color:{DIM}; letter-spacing:.05em; }}
.dhint {{ color:{DIM}; font-size:13px; }}
.mono {{ font-family:'SF Mono',ui-monospace,monospace; }}
/* split handle — ONE consistent 2px line, SAME color (#3a4150), in BOTH the overview sidebar split and
   the terminal splits. (The `.dside` no longer draws its own edge line, so there is no double-thickness.)
   Hover is only a SUBTLE lighter grey — never the bright accent, which read as a big blue bar. */
paned > separator {{ background-color:#3a4150; min-width:2px; min-height:2px; padding:0; margin:0; -gtk-icon-source:none; }}
paned > separator:hover {{ background-color:#4a5262; }}
/* terminal: a little inset so the leftmost column is selectable + not flush to the edge */
vte-terminal, terminal {{ padding:3px 6px 3px 8px; }}
/* copy/scroll-mode: a subtle accent frame so the mode is visible */
vte-terminal.copymode, terminal.copymode {{ box-shadow: inset 0 0 0 1px {ACCENT}; }}

/* ---- search bar (Cmd+F) — slim, black, floats top-right over the terminal ---- */
	.searchbar {{ background-color:{BG1}; border:1px solid {CONTROL_LINE}; border-top:none; border-radius:0 0 9px 9px; padding:6px 8px; margin:0 10px 0 0; box-shadow:0 4px 14px rgba(0,0,0,.4); }}
	.searchfield {{ background-color:{BG2}; color:{TXT}; border:1px solid {CONTROL_LINE}; border-radius:6px; padding:4px 8px; min-height:0; font-size:12.5px; caret-color:{ACCENT}; }}
.searchfield:focus {{ border-color:{ACCENT}; }}
.searchinfo {{ font-size:11px; color:{FAINT}; min-width:56px; }}
.searchinfo.nomatch {{ color:#ff6b6b; }}
"
	)
}

#[cfg(test)]
mod tests {
    use super::{css, ACCENT_FILL, ACCENT_FILL_HOVER, BG0, BG1, BG2, CONTROL_LINE, FAINT};

    fn channel(value: u8) -> f64 {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(color: &str) -> f64 {
        let component = |offset| u8::from_str_radix(&color[offset..offset + 2], 16).unwrap();
        0.2126 * channel(component(1)) + 0.7152 * channel(component(3)) + 0.0722 * channel(component(5))
    }

    fn contrast(left: &str, right: &str) -> f64 {
        let (light, dark) = if luminance(left) > luminance(right) {
            (luminance(left), luminance(right))
        } else {
            (luminance(right), luminance(left))
        };
        (light + 0.05) / (dark + 0.05)
    }

    #[test]
    fn theme_never_suppresses_toolkit_focus_indicators() {
        let theme = css();
        assert!(!theme.contains("outline:none"));
        assert!(!theme.contains("outline: none"));
    }

    #[test]
    fn faint_normal_text_meets_contrast_on_application_surfaces() {
        for background in [BG0, BG1, BG2] {
            assert!(contrast(FAINT, background) >= 4.5);
        }
    }

    #[test]
    fn white_control_text_meets_contrast_in_normal_and_hover_states() {
        for background in [ACCENT_FILL, ACCENT_FILL_HOVER] {
            assert!(contrast("#ffffff", background) >= 4.5);
        }
    }

    #[test]
    fn control_boundaries_meet_non_text_contrast() {
        assert!(contrast(CONTROL_LINE, BG2) >= 3.0);
    }
}
