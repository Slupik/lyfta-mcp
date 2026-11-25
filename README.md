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

`lyfta-mcp --env-file PATH --http 127.0.0.1:18202 --allowed-host mcp.example.com` serves stateless streamable HTTP at `/mcp`. The server has no authentication of its own. To protect it, bind it to loopback and put an authenticating proxy in front (for example Cloudflare Access with Managed OAuth). Requests whose `Host` header is not loopback or an `--allowed-host` value are rejected.

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
| `lyfta_preview_program` | Review all days together, resolve catalog matches, and inspect proposed loads and their sources. |
| `lyfta_load_program_draft` | Copy an existing program into an editable draft for a new block. |
| `lyfta_get_comparable_sets` | Read recent completed sets for comparable exercise IDs selected from the user's training history. |
| `lyfta_save_program`, `lyfta_resume_program_save` | Save an approved preview, or continue it using the same preview ID. |
| `lyfta_sample_workouts` | Check the connection against Lyfta's public example data. |

Exercise search, performed exercises, previews and source sets include readable equipment, body part and muscle labels using the [Lyfta ID mappings](https://my.lyfta.app/community/api#id-mappings). Where the API supplies an unfamiliar ID, the label includes that number. Exercise catalog IDs still identify the entries used in previews and saved routines.

## Summarizing training

Call `lyfta_get_training_summary` with inclusive UTC dates `from` and `to` in YYYY-MM-DD format. It reads all matching workout pages and groups Lyfta's recorded volume by Monday-start week and weight unit. Each completed set counts once for every body part Lyfta tags on its exercise. Sets with logged reps, duration, or distance count when the completion flag is missing or the session has no completed flags. The report shows how many sets were inferred this way. The report includes counts for sessions with missing dates, weeks with missing volume values, and sets without a body part.

## Planning a program in ChatGPT

Connect ChatGPT to this server using the HTTP setup above. Paste a plan or describe it in the chat. For example:

> Plan a two-day block in kg. On Monday, bench press 3×8, with 45 kg on the first set, and dumbbell bench press 2×12 as a new exercise. On Thursday, full squat 3×8. Use my history for missing loads where possible. Show me both days so I can approve the plan before saving it.

1. ChatGPT converts the plan to `lyfta_preview_program` arguments. Send `collection: {"title":"Two-day block"}`, and put each named routine in `days` with its exercise list in order. Use one object per planned set in `sets`, for example `[{"reps":8,"weight":45},{"reps":8},{"reps":8}]`. Omit blank weights or use `null`. For this example send `use_history: true` and `weight_unit: "kg"`. The unit must match the destination account's Lyfta setting.
2. If the catalog match is ambiguous or approximate, show candidates with their names, equipment and IDs. After the user chooses, set `name` and `exercise_id` on that exercise and preview again. For the same exercise, missing loads can use a completed set from the latest session with a logged load and reps. The preview includes the source date, weight and reps. When there are several eligible sets in that session, it prefers reps closest to the planned set. An entered load takes precedence.
3. For a new lift, choose likely comparable movements from `lyfta_list_performed_exercises`. Call `lyfta_get_comparable_sets` with the new catalog ID as `target_exercise_id` and the selected historical exercise IDs as `comparable_exercise_ids`. The result contains recent completed sets for those IDs, with equipment, weight, unit, reps, date, and RIR when available. A similar name or muscle group alone is insufficient. Consider the movement, equipment, goal reps and RIR. Suggest a conservative starting load or range grounded in the evidence. If there are no suitable sets, leave the weight blank and suggest a light trial set. An optional `from` date bounds the history read. Use the same date as `history_from` in the preview if it applies to the source sets.
4. Attach the suggestion as an `estimate` on each applicable set. Use `weight` for a single proposed load or `lower` and `upper` for a range. For example, given a hypothetical completed barbell bench set of 60 kg × 10 at 2 RIR, ChatGPT might propose 14–18 kg per hand for a first dumbbell bench session at 12 reps. Use the returned source references verbatim. The values below are illustrative.

   ```json
   {
     "reps": 12,
     "estimate": {
       "unit": "kg",
       "load_basis": "per_hand",
       "lower": 14,
       "upper": 18,
       "reason": "Start conservatively at 12 reps with independent dumbbells and greater stability demands.",
       "source_refs": ["12/2/one"]
     }
   }
   ```

   The preview marks this as an estimate and displays the actual source sets. For a range, ask the user to choose a load and add `selected_weight` within the range before previewing again. The API writes the selected number in the routine's weight field. `load_basis` describes the number in the preview and may be `per_hand`, `total`, or `unknown`. Unknown conventions in the source log stay unknown. Equipment alone does not establish how the lifter logged the weight. This workflow uses no fixed conversions across equipment.
5. Show the final, ready preview of both days, including the names that will appear in Lyfta and any blank loads. When the user approves that preview, call `lyfta_save_program` with `{"preview_id":"<from preview>","approved":true}`. The result includes the new collection ID and the IDs of saved routines.

For a new block based on a saved program, call `lyfta_load_program_draft` with its `collection_id` and a new `title`. Edit the returned `draft`, including any changes to exercise order, sets or loads. Remove the old `exercise_id` when replacing an exercise by name. Submit the edited draft to `lyfta_preview_program` and follow the same review and approval flow.

### Resuming a save

Keep the `preview_id` and call `lyfta_resume_program_save` with it if saving stops. The reply includes the collection ID once known and IDs of all confirmed routines. Save progress is stored in a local journal next to the environment file in `.lyfta-programs/`. Set `LYFTA_PROGRAM_STATE_DIR` in the server's environment to use another writable directory. Keep that directory across restarts and use the same journal directory for requests that can resume the same operation.

Lyfta's public API has no documented idempotency key. The preview shows a unique suffix added to each saved title so the server can find the exact item if Lyfta accepts a write and the reply is lost. Keep those titles in Lyfta until the save finishes. If a write is unconfirmed and the item is absent from the library, the resume call returns `awaiting_reconciliation`. Subsequent resume calls check again without repeating the uncertain write. If the item never becomes visible, the server cannot establish whether repeating that write would be safe. A definitive rejection such as a rate limit allows the same preview to resume the write after the condition clears.

## Saving individual routines

The [public API specification](https://my.lyfta.app/openapi.json) documents creation of collections and templates. Editing an existing collection or template in place, deleting it, and changing the calendar are outside those endpoints.

To save a revision, read the original routine with `lyfta_get_template`, then use `lyfta_create_template` to save a named revision in its collection or a new collection. Use the exact exercise ID, name, type, and image from Lyfta's exercise tools. A template requires an existing `collectionId` and Lyfta's request format stores set values such as reps and weight as strings.

To draft a routine from exercise names, call `lyfta_preview_template` with the destination `collectionId` and a `workout` whose exercises use `name`. Each exercise accepts the same sets, rest time, notes, and superset fields as creation. The preview matches exact names ignoring case and repeated whitespace. For an approximate or ambiguous name, choose a returned candidate by its catalog name and `exercise_id`, then preview again. When all exercises match and have the required catalog metadata, review `create_template_arguments` and pass it to `lyfta_create_template` to save the routine. Previewing only reads from Lyfta.

Each create call sends one POST and returns the new item's ID. Lyfta does not document an idempotency key for these endpoints. After an interrupted or ambiguous write, inspect the program or list your collections before submitting the create again.
