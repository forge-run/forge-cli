# {{workspace_name}}

A git-native Forge **workspace** whose ops are written in {{dialect_name}}.
`main` is the desired state: a `git push` is checked and served, with nothing
built on your side.

## Layout

| Path | What |
|------|------|
| `workspace.json` | workspace identity + host→app routing |
| `domains/{{domain}}/service.json` | the domain's op declarations |
| `domains/{{domain}}/services/{{op_file}}` | the starter op, `{{domain}}::hello` |
| `domains/{{domain}}/schemas/notes.table.json` | the `notes` table: one `body` per row, each user reading the rows they wrote |
| `domains/{{domain}}/services/{{list_file}}` | `{{domain}}::list_notes`, the notes the caller may read |
| `domains/{{domain}}/services/{{add_file}}` | `{{domain}}::add_note`, which writes one |
| `apps/web/pages/index.tsx` | the page at `/`: its loader calls `hello` and it renders the answer |
| `apps/web/layouts/app.tsx` | the layout `/notes` and `/wave` sit in: a nav, a progress bar while a page loads, and the page |
| `apps/web/pages/notes.tsx` | the page at `/notes`, a client page whose list of notes is live |
| `apps/web/pages/wave.tsx` | the page at `/wave`, a client page: the browser runs the same loader as `/` and renders it |
| `apps/web/components/note_composer.tsx` | the form that adds a note, reading "Saving…" while `add_note` is in flight |
| `apps/web/components/greeting.tsx` | the component `/` and `/wave` render the answer through |
| `apps/web/components/wave.tsx` | a button holding a state cell its click sets, which only a client page may render |
| `schema.lock` | the compiled schema the checker reads |
| `.forge/skill/{{skill_file}}` | the {{dialect_name}} subset forge accepts — hand it to your coding agent before it writes an op (git-ignored; `forge new` wrote it from this CLI's own checker) |
| `.forge/types/` | the schema surface your editor resolves (git-ignored) |

## Write an op with a model

Give your agent `.forge/skill/{{skill_file}}` and the task. The skill is the
accepted subset, generated from the same register the checker refuses from,
so every refusal it can meet is named in it with what to write instead.

## Pages and components

The UI is TSX, whatever language the ops are written in. A `.tsx` file under
`apps/<app>/pages/` is a page, routed by where it sits (`index.tsx` is `/`,
`about.tsx` is `/about`); one under `components/` is a component. A component
declares its props as an `interface` and default-exports one function from
them to `Node`. A page's data comes only from the ops its `loader` names, each
run as the requesting user and typed by the op's generated `<Op>Response`.
A page renders on the server; nothing is built, bundled or installed.

A page that declares `export const mode = "client";` is a client page, like
`wave.tsx`: the server answers it with a shell, and the browser fetches the
loader's ops from their REST routes as the signed-in user and renders the
page. Its components may hold state cells, `const [waves, setWaves] =
useState<number>(0);`, and click handlers, `onClick={() => setWaves(waves + 1)}`,
whose only effects are setting a cell and calling an op with
`action("<op>", { … })`. A server page refuses both.

## The client app

`/notes` and `/wave` are client pages in one layout,
`export const layout = AppLayout;`. A link between them swaps the page in
place, with no document load, while `navigation.pending` shows the layout's
progress bar. `/notes` loads `list_notes` with `{ live: true }`: when a row of
`notes` the viewer may read changes, in this tab or another, the page fetches
the list again and repaints. The composer posts to `add_note` and reads
`isPending(action("add_note"))`, so its button is disabled and says
"Saving…" until the note is written.

A live load reads only what the viewer's row-level policy admits, and a table
with no select policy admits nothing to a signed-in user, so `notes` declares
one: a user reads the notes they wrote (`created_by` is `$user_id`). Two
browsers signed in as the same user see each other's notes arrive; another
user sees only their own. `schema.lock` must declare every table a live load
reads, so run `forge schema compile` after you add one.

`hello` is declared `authenticated`, so a visitor who has not signed in sees
the page's failure arm. The components section of
`.forge/skill/SUBSET-SKILL-TS.md`, which `forge new` writes whatever the ops'
dialect, is the subset a page or component may use.

## Check and push

```bash
forge check                 # the verdict the push will give, locally: ops, pages, components
forge login
forge ws use <workspace-id>
git remote add forge https://git.forge.run/<workspace-id>/{{workspace_name}}
git push forge main
```
