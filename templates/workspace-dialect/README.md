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
| `apps/web/pages/index.tsx` | the page at `/`: its loader calls `hello` and it renders the answer |
| `apps/web/pages/wave.tsx` | the page at `/wave`, a client page: the browser runs the same loader and renders it |
| `apps/web/components/greeting.tsx` | the component both pages render the answer through |
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
