// Direction C — Vinyl Crate
// Disc / groove motif. Honey hairlines as groove dividers.

function DirectionC({ state = "primary" }) {
  const showSettings = state === "settings" || state === "db-missing";
  const dbMissing = state === "db-missing";
  const isEmpty = state === "no-notes";
  const noTracks = state === "no-tracks";
  const isEditing = state === "editing";

  return (
    <div className="bc-plugin dirC">
      <header className="c-header">
        <div className="c-disc" aria-hidden="true" />
        <div className="c-wordmark">
          Beat<span className="c-accent">Crate</span>
        </div>
        <button className="c-iconbtn" title="Settings">
          <Ico.Gear size={15} />
        </button>
        <button className="c-textbtn">Refresh</button>
      </header>

      <div className="c-spin">
        <span className="c-status-dot" />
        <span className="c-status-text">{dbMissing ? "no record" : "33⅓ rpm"}</span>
        <span style={{ color: "var(--fg-ghost)" }}>·</span>
        <span>{dbMissing ? "locate beatcrate.db" : "14 crates loaded"}</span>
      </div>

      <div className="c-form">
        <div>
          <div className="c-fieldlabel">Crate</div>
          <div className="c-select focus">
            <span style={{ flex: "1 1 auto" }}>Wander Foster — Side A</span>
            <span className="c-caret"><Ico.Caret /></span>
          </div>
        </div>
        <div>
          <div className="c-fieldlabel">Track</div>
          <div className="c-select">
            <span style={{ flex: "1 1 auto" }}>
              {noTracks
                ? <span className="c-empty">crate is empty</span>
                : "MW-001 (rough mix)"}
            </span>
            <span className="c-caret"><Ico.Caret /></span>
          </div>
        </div>
      </div>

      <div className="c-section-head">
        <span className="c-side">SIDE B</span>
        <span>Notes</span>
        <span className="c-rule" />
        <span className="c-count">
          {noTracks ? "—" : isEmpty ? "0" : `${NOTES.filter(n => n.done).length} / ${NOTES.length}`}
        </span>
      </div>

      {noTracks || isEmpty ? (
        <div className="c-empty-state">
          <div className="c-empty-disc" />
          <div className="c-empty-title">{noTracks ? "no tracks pressed" : "no grooves cut yet"}</div>
          <div className="c-empty-hint">
            {noTracks
              ? "This crate is empty. Add tracks in the desktop app, then refresh."
              : "Drop a needle and write a note. They sync back to the crate."}
          </div>
        </div>
      ) : (
        <div className="c-list bc-scroll">
          {NOTES.map((n, i) => (
            isEditing && i === 2 ? (
              <div key={n.id} className="c-row editing">
                <div className="c-check" />
                <div>
                  <input className="c-edit" defaultValue={n.text} autoFocus />
                  <div className="c-edit-hints">
                    <span><kbd>↵</kbd> save</span>
                    <span><kbd>esc</kbd> cancel</span>
                  </div>
                </div>
              </div>
            ) : (
              <div key={n.id} className={`c-row ${n.done ? "done" : ""} ${i === 0 ? "playing" : ""}`}>
                <div className="c-check" />
                <div className="c-text">{n.text}</div>
              </div>
            )
          ))}
        </div>
      )}

      <form className="c-input-row" onSubmit={(e) => e.preventDefault()}>
        <span className="c-input-needle" />
        <input className="c-input" placeholder="cut a new note…" />
        <button type="submit" className="c-submit">Press</button>
      </form>

      {showSettings && (
        <div className="c-settings">
          <div className="c-settings-head">
            <div className="c-settings-title">
              <span className="c-disc" style={{ width: 18, height: 18 }} />
              <span>Settings</span>
            </div>
            <button className="c-settings-close" title="close"><Ico.X size={14} /></button>
          </div>
          <div className="c-settings-body">
            <div className="c-settings-section">
              <div className="c-settings-label">Database</div>
              <div className={`c-settings-pathbox ${dbMissing ? "missing" : ""}`}>
                {dbMissing ? "— beatcrate.db not located —" : "~/Music/BeatCrate/beatcrate.db"}
              </div>
              {dbMissing ? (
                <div className="c-settings-helper">
                  Point this at the <span style={{ fontFamily: "var(--mono)", color: "var(--accent-hi)" }}>beatcrate.db</span> your desktop app writes to — usually in <span style={{ fontFamily: "var(--mono)" }}>~/Music/BeatCrate</span>.
                </div>
              ) : (
                <div className="c-settings-helper">
                  The plugin reads notes from this database. Same file your desktop app uses.
                </div>
              )}
              <div className="c-settings-row">
                <button className={`c-settings-btn ${dbMissing ? "primary" : ""}`}>
                  {dbMissing ? "Locate beatcrate.db…" : "Change…"}
                </button>
                {!dbMissing && <button className="c-settings-btn">Reveal in Finder</button>}
              </div>
            </div>
            <div className="c-settings-future">— more on the b-side —</div>
          </div>
        </div>
      )}
    </div>
  );
}

window.DirectionC = DirectionC;
