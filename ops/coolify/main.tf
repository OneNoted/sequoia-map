# The production Sequoia Map application, adopted from Coolify by `tofu import`. Only the
# settings below are managed; everything else stays as configured in Coolify, because the
# provider writes only attributes whose planned value differs from state.

# Credentials come from COOLIFY_ENDPOINT and COOLIFY_TOKEN.
provider "coolify" {}

resource "coolify_application" "map" {
  project_uuid     = var.project_uuid
  server_uuid      = var.server_uuid
  environment_name = "production"

  git_repository          = "https://github.com/OneNoted/sequoia-map.git"
  git_branch              = "main"
  build_pack              = "dockercompose"
  docker_compose_location = var.docker_compose_location
  ports_exposes           = "3000"

  # Deploys come from the Deploy Images workflow, never from a push or from this
  # configuration.
  is_auto_deploy_enabled = false
  instant_deploy         = false
  redeploy_on_update     = false

  lifecycle {
    prevent_destroy = true
    # Coolify keeps its legacy /Dockerfile default on this Compose application; Compose
    # deploys never read it, and this module does not manage it.
    ignore_changes = [dockerfile_location]
  }
}
