// Standalone page wrapper — toggle between states + sizes for one direction.

const STANDALONE_STATES = [
  { id: "primary",    label: "With notes" },
  { id: "no-notes",   label: "Empty" },
  { id: "no-tracks",  label: "Crate empty" },
  { id: "editing",    label: "Inline edit" },
  { id: "settings",   label: "Settings" },
  { id: "db-missing", label: "DB missing" },
];

const STANDALONE_SIZES = [
  { id: "s",  label: "400 × 500", w: 400, h: 500 },
  { id: "m",  label: "480 × 560", w: 480, h: 560 },
  { id: "l",  label: "600 × 800", w: 600, h: 800 },
];

function StandalonePage({ direction, title, subtitle, Component }) {
  const [state, setState] = React.useState("primary");
  const [size, setSize] = React.useState("m");
  const sz = STANDALONE_SIZES.find(s => s.id === size);

  return (
    <div className="standalone">
      <div className="standalone-head">
        <div className="standalone-title">{title}</div>
        <div className="standalone-subtitle">{subtitle}</div>
      </div>
      <div className="standalone-toolbar">
        <div className="standalone-group">
          <span className="standalone-grouplabel">State</span>
          {STANDALONE_STATES.map(s => (
            <button key={s.id}
              className={`standalone-pill ${state === s.id ? "active" : ""}`}
              onClick={() => setState(s.id)}>
              {s.label}
            </button>
          ))}
        </div>
        <div className="standalone-group">
          <span className="standalone-grouplabel">Size</span>
          {STANDALONE_SIZES.map(s => (
            <button key={s.id}
              className={`standalone-pill ${size === s.id ? "active" : ""}`}
              onClick={() => setSize(s.id)}>
              {s.label}
            </button>
          ))}
        </div>
      </div>
      <div className="standalone-stage">
        <div className="standalone-frame" style={{ width: sz.w, height: sz.h }}>
          <Component state={state} />
        </div>
      </div>
      <div className="standalone-foot">
        <a href="../../index.html">← all three directions on the canvas</a>
      </div>
    </div>
  );
}

window.StandalonePage = StandalonePage;
