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
| `schema.lock` | the compiled schema the checker reads |
| `.forge/skill/{{skill_file}}` | the {{dialect_name}} subset forge accepts — hand it to your coding agent before it writes an op (git-ignored; `forge new` wrote it from this CLI's own checker) |
| `.forge/types/` | the schema surface your editor resolves (git-ignored) |

## Write an op with a model

Give your agent `.forge/skill/{{skill_file}}` and the task. The skill is the
accepted subset, generated from the same register the checker refuses from,
so every refusal it can meet is named in it with what to write instead.

## Check and push

```bash
forge check                 # the verdict the push will give, locally
forge login
forge ws use <workspace-id>
git remote add forge https://git.forge.run/<workspace-id>/{{workspace_name}}
git push forge main
```
