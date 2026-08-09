/* Sonda docs — CodeMirror editor factory.
 *
 * Source for the vendored bundle at docs/site/docs/javascripts/sonda-editor.js
 * (rebuild with `task site:editor`). Exposes one factory the playground uses;
 * CodeMirror itself never leaks into the page scripts.
 */
import { EditorView, keymap, lineNumbers, highlightActiveLine, drawSelection } from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { yaml } from "@codemirror/lang-yaml";
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
});

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
