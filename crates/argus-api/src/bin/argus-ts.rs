//! Writes the generated TypeScript to stdout:
//! `cargo run -p argus-api --bin argus-ts > ui/packages/argus-client/src/gen/api.ts`.
//! CI runs it and fails on a diff, so the TS can never lag the op list.

fn main() {
    print!("{}", argus_api::typescript());
}
