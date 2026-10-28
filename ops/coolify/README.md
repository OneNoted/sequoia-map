# Coolify application (OpenTofu)

This directory manages the production Coolify application that deploys `compose.yaml`. It
uses OpenTofu 1.13.2 and the `coolify-terraform/coolify` provider 0.1.28 from `registry.terraform.io`, both pinned.

The application already exists, so this configuration adopts it by import and never creates
it. It manages:
- the repository, branch and build pack;
- the Compose file location;
- deploys off: no auto-deploy, no deploy on apply and no restart on update.

Everything else, including domains, environment variables and resource limits, stays as
configured in Coolify. The provider writes only attributes whose planned value differs from
state. `prevent_destroy` guards the stateful stack.

## Inputs

| Input | What |
|---|---|
| `SEQUOIA_COOLIFY_STATE` | Absolute path of the private state file, the same for every checkout. Beside it go the encrypted saved plan (`<state>.tfplan`) and OpenTofu's working data (`<state>.data`), all owner-only |
| `SEQUOIA_COOLIFY_VARS` | A private tfvars file with `project_uuid` and `server_uuid` (see `operator.tfvars.example`) |
| `TF_VAR_state_passphrase` | Encrypts the state and saved plans (PBKDF2, AES-GCM); unencrypted state and plans are refused |
| `COOLIFY_ENDPOINT`, `COOLIFY_TOKEN` | The provider's credentials |

Keep the state, plans and tfvars out of the repository. Each command stops with the name of
any input it is missing.

## Commands

```bash
mise run coolify:init                                          # once per state file and provider version
mise run coolify:import -- <project>:<server>:production:<app> # once, adopts the application
mise run coolify:plan                                          # writes the encrypted saved plan
mise run coolify:apply                                         # applies only that saved plan
```

Read the whole plan before applying it.

1. **After import, confirm no changes.** Plan with `docker_compose_location` set in the
   tfvars to the application's current path. It should report no changes.
2. **Then plan the real change.** Remove that override, so the default `/compose.yaml`
   applies, and plan again. Only an in-place change of `docker_compose_location` should
   appear.

Apply changes only the stored setting. The next deployment, from Deploy Images, reads the new
path.
