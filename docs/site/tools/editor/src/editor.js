/* Sonda docs — CodeMirror editor factory.
 *
 * Source for the vendored bundle at docs/site/docs/javascripts/sonda-editor.js
 * (rebuild with `task site:editor`). Exposes one factory the playground uses;
 * CodeMirror itself never leaks into the page scripts.
 */
import {
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLine,
  drawSelection,
  Decoration,
  MatchDecorator,
  ViewPlugin,
} from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { yaml } from "@codemirror/lang-yaml";
import { numberSpanAt, scrubNumber } from "../../../docs/javascripts/sonda-pure.js";
import {
  syntaxHighlighting,
  defaultHighlightStyle,
  bracketMatching,
  indentUnit,
} from "@codemirror/language";
import { setDiagnostics, lintGutter } from "@codemirror/lint";
import { oneDark } from "@codemirror/theme-one-dark";

/* Shared chrome for both schemes — sizing and fonts come from the page,
 * colors from the theme compartment below. */
const baseTheme = EditorView.theme({
  "&": {
    fontSize: "0.76rem",
    borderRadius: "8px",
    border: "1px solid var(--md-default-fg-color--lightest)",
    minHeight: "26rem",
  },
  "&.cm-focused": { outline: "2px solid var(--sonda-accent, #f97316)", outlineOffset: "1px" },
  ".cm-scroller": {
    fontFamily: '"JetBrains Mono", ui-monospace, monospace',
    lineHeight: "1.6",
  },
  ".cm-content": { padding: "0.75rem 0" },
  ".cm-gutters": { borderRadius: "8px 0 0 8px" },
  ".cm-scrub-number": {
    cursor: "ew-resize",
    borderBottom: "1px dashed var(--sonda-accent, #f97316)",
  },
});

/* --- param scrubbing ---------------------------------------------------
 *
 * Numbers that stand alone as YAML scalar values (numberSpanAt decides —
 * `amplitude: 30.0` yes, `host: web-01` no) get a dashed underline and an
 * ew-resize cursor; dragging one horizontally rewrites the literal in
 * place, one scrub step per few pixels. Each rewrite is an ordinary
 * document change, so the playground's updateListener → debounced run
 * redraws the chart live as the value moves.
 */

const SCRUB_PIXELS_PER_STEP = 4;
const SCRUB_DEAD_ZONE_PX = 3;

const scrubMark = Decoration.mark({ class: "cm-scrub-number" });

const scrubDecorator = new MatchDecorator({
  regexp: /-?\d+(?:\.\d+)?/g,
  decorate: (add, from, to, match, view) => {
    const line = view.state.doc.lineAt(from);
    const span = numberSpanAt(line.text, from - line.from);
    if (span && line.from + span.start === from) add(from, to, scrubMark);
  },
});

const scrubHighlighter = ViewPlugin.fromClass(
  class {
    constructor(view) {
      this.decorations = scrubDecorator.createDeco(view);
    }
    update(update) {
      this.decorations = scrubDecorator.updateDeco(update, this.decorations);
    }
  },
  { decorations: (plugin) => plugin.decorations }
);

/* Mousedown over a scrubbable number claims the gesture: a horizontal drag
 * scrubs, a plain click (no movement past the dead zone) still places the
 * cursor. Selection by dragging must start OUTSIDE a number — double-click
 * and keyboard selection are untouched (detail > 1 falls through), and so
 * is any modifier-click. */
const scrubGesture = EditorView.domEventHandlers({
  mousedown: (event, view) => {
    if (event.button !== 0 || event.detail !== 1) return false;
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return false;
    const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
    if (pos === null) return false;
    const line = view.state.doc.lineAt(pos);
    const span = numberSpanAt(line.text, pos - line.from);
    if (!span) return false;
    beginScrub(view, event, line.from + span.start, span.text, pos);
    return true;
  },
});

function beginScrub(view, event, from, original, clickPos) {
  event.preventDefault();
  const startX = event.clientX;
  let applied = original;
  let active = false;
  const previousCursor = document.body.style.cursor;

  const move = (moveEvent) => {
    const dx = moveEvent.clientX - startX;
    if (!active && Math.abs(dx) < SCRUB_DEAD_ZONE_PX) return;
    if (!active) {
      active = true;
      document.body.style.cursor = "ew-resize";
    }
    moveEvent.preventDefault();
    // Steps always derive from the gesture's ORIGINAL literal, so the step
    // size stays fixed and dragging back to the start restores the value.
    const next = scrubNumber(original, Math.round(dx / SCRUB_PIXELS_PER_STEP));
    if (next === applied) return;
    view.dispatch({
      changes: { from, to: from + applied.length, insert: next },
      userEvent: "input.scrub",
    });
    applied = next;
  };

  const up = () => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
    document.body.style.cursor = previousCursor;
    // The mousedown was consumed, so restore click-to-place-cursor by hand.
    if (!active) view.dispatch({ selection: { anchor: clickPos } });
    view.focus();
  };

  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
}

const lightTheme = [syntaxHighlighting(defaultHighlightStyle)];
const darkTheme = [oneDark];

/**
 * Create a YAML scenario editor.
 *
 * options: { parent, doc, dark, onChange(), onRun() }
 * returns: { getValue(), setValue(text), setDark(bool),
 *            setEngineError(message|null), focus() }
 */
export function createScenarioEditor(options) {
  const themeCompartment = new Compartment();

  const view = new EditorView({
    parent: options.parent,
    state: EditorState.create({
      doc: options.doc || "",
      extensions: [
        lineNumbers(),
        history(),
        drawSelection(),
        highlightActiveLine(),
        bracketMatching(),
        indentUnit.of("  "),
        yaml(),
        lintGutter(),
        scrubHighlighter,
        scrubGesture,
        baseTheme,
        themeCompartment.of(options.dark ? darkTheme : lightTheme),
        keymap.of([
          {
            key: "Mod-Enter",
            run: () => {
              if (options.onRun) options.onRun();
              return true;
            },
          },
          indentWithTab,
          ...defaultKeymap,
          ...historyKeymap,
        ]),
        EditorView.updateListener.of((update) => {
          if (update.docChanged && options.onChange) options.onChange();
        }),
      ],
    }),
  });

  return {
    getValue: () => view.state.doc.toString(),
    setValue: (text) => {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
    },
    setDark: (dark) => {
      view.dispatch({ effects: themeCompartment.reconfigure(dark ? darkTheme : lightTheme) });
    },
    setEngineError: (message) => {
      view.dispatch(setDiagnostics(view.state, message ? engineErrorToDiagnostics(view.state, message) : []));
    },
    focus: () => view.focus(),
  };
}

/* The engine's YAML errors carry "at line N column M" (serde_yaml_ng); pin
 * the diagnostic to that line. Errors without a position mark line 1 so the
 * message still shows in the gutter. */
function engineErrorToDiagnostics(state, message) {
  const match = message.match(/at line (\d+) column (\d+)/);
  let from = 0;
  let to = 0;
  if (match) {
    const lineNumber = Math.min(Number(match[1]), state.doc.lines);
    const line = state.doc.line(lineNumber);
    const column = Math.min(Number(match[2]) - 1, Math.max(0, line.length - 1));
    from = line.from + Math.max(0, column);
    to = line.to;
  } else {
    to = Math.min(state.doc.length, state.doc.line(1).to);
  }
  if (to < from) to = from;
  return [{ from, to, severity: "error", message }];
}
