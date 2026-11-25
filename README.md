# Lyfta MCP

A local stdio MCP server for the [official Lyfta Developer API](https://my.lyfta.app/community/api). It works with the personal Lyfta library attached to the configured API key.

## Setup

1. Install Rust and Cargo 1.88 or newer and run `cargo build --locked` in this directory.
2. To access your account, sign in to [Lyfta API Access](https://my.lyfta.app/community/api) and generate a personal API key. Save it in an `.env` file in this directory as `LYFTA_API_KEY=your_key_here` or inject it into the MCP server's environment. Git ignores the `.env` file.
3. Configure your MCP client to launch the server over stdio. Replace `/absolute/path/to/lyfta` with this directory. Use `target/debug/lyfta-mcp.exe` on Windows.

```json
{
  "mcpServers": {
    "lyfta": {
      "command": "/absolute/path/to/lyfta/target/debug/lyfta-mcp",
      "args": [
        "--env-file",
        "/absolute/path/to/lyfta/.env"
      ]
    }
  }
}
```

Call `lyfta_sample_workouts` to try the public sample endpoint without a key. Use `lyfta_list_workouts` with `limit: 1` to verify account access. From this directory you can also launch the server with `cargo run --locked`. The server looks for `.env` in the current directory when `--env-file` is omitted. A `LYFTA_API_KEY` supplied directly in the environment takes precedence.

## Remote (HTTP) mode

`lyfta-mcp --env-file PATH --http 127.0.0.1:18202 --allowed-host mcp.example.com` serves stateless streamable HTTP at `/mcp`. The server has no authentication of its own: bind it to loopback and put an authenticating proxy in front (for example Cloudflare Access with Managed OAuth). Requests whose `Host` header is not loopback or an `--allowed-host` value are rejected.

Build a static Linux binary on macOS with `cargo zigbuild --release --locked --target x86_64-unknown-linux-musl`.

## Tools

| Tool | Use |
| --- | --- |
| `lyfta_list_workouts` | Read completed sessions with exercises, sets, reps, and weights. Supports pagination and inclusive UTC date filters. |
| `lyfta_list_workout_summaries` | Browse a lighter history with duration and volume. |
| `lyfta_get_training_summary` | Count sessions, see weekly volume by weight unit, and count completed sets by body part for a date range. |
| `lyfta_list_performed_exercises`, `lyfta_search_exercise_library` | Find exercises you logged or search the full catalog. |
| `lyfta_get_exercise_progress` | Look up best sets and estimated one-rep max for an exercise. |
| `lyfta_list_schedule` | See scheduled sessions, optionally including completed sessions. |
| `lyfta_list_collections`, `lyfta_get_collection` | Browse saved programs and their routines. |
| `lyfta_list_templates`, `lyfta_get_template` | Browse saved routines and read their exercises and sets. |
| `lyfta_create_collection` | Create a program with a title, optional description, goal, and cover. |
| `lyfta_preview_template` | Match exercise names and review a draft routine before saving. |
| `lyfta_create_template` | Add a new routine to a specific program, including exercise order, sets, rep ranges, loads, rest, and supersets. |
| `lyfta_sample_workouts` | Check the connection against Lyfta's public example data. |

## Summarizing training

Call `lyfta_get_training_summary` with inclusive UTC dates `from` and `to` in YYYY-MM-DD format. It reads all matching workout pages and groups Lyfta's recorded volume by Monday-start week and weight unit. Each completed set counts once for every body part Lyfta tags on its exercise. Sets with logged reps, duration, or distance count when the completion flag is missing or the session has no completed flags. The report shows how many sets were inferred this way. The report includes counts for sessions with missing dates, weeks with missing volume values, and sets without a body part.

## Saving revised plans

The [public API specification](https://my.lyfta.app/openapi.json) documents creation of collections and templates. Editing an existing collection or template in place, deleting it, and changing the calendar are outside those endpoints.

To save a revision, read the original routine with `lyfta_get_template`, then use `lyfta_create_template` to save a named revision in its collection or a new collection. Use the exact exercise ID, name, type, and image from Lyfta's exercise tools. A template requires an existing `collectionId` and Lyfta's request format stores set values such as reps and weight as strings.

To draft a routine from exercise names, call `lyfta_preview_template` with the destination `collectionId` and a `workout` whose exercises use `name`. Each exercise accepts the same sets, rest time, notes, and superset fields as creation. The preview matches exact names ignoring case and repeated whitespace. For an approximate or ambiguous name, choose a returned candidate by its catalog name and `exercise_id`, then preview again. When all exercises match and have the required catalog metadata, review `create_template_arguments` and pass it to `lyfta_create_template` to save the routine. Previewing only reads from Lyfta.

Each create call sends one POST and returns the new item's ID. Lyfta does not document an idempotency key for these endpoints. After an interrupted or ambiguous write, inspect the program or list your collections before submitting the create again.
