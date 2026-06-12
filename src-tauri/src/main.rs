#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Headless DB smoke test: `BeatCrate --check-db` opens + migrates the DB at
    // the resolved path (honors BEATCRATE_DATA_DIR), prints counts, and exits
    // without launching the window. Used to verify db.rs against a throwaway copy.
    if std::env::args().any(|a| a == "--check-db") {
        beatcrate_lib::check_db();
        return;
    }
    // `BeatCrate --verify-ingest` exercises task #4 (ingest + .als index +
    // loudness) against the resolved DB. Point BEATCRATE_DATA_DIR at a COPY.
    if std::env::args().any(|a| a == "--verify-ingest") {
        beatcrate_lib::verify_ingest();
        return;
    }
    beatcrate_lib::run()
}
