terraform {
  required_version = "1.13.2"

  required_providers {
    coolify = {
      source  = "registry.terraform.io/coolify-terraform/coolify"
      version = "0.1.28"
    }
  }

  # The state file lives at one private operator path, given to `tofu init` as
  # -backend-config=path=..., so every checkout manages the same state.
  backend "local" {}

  encryption {
    key_provider "pbkdf2" "state" {
      passphrase = var.state_passphrase
    }

    method "aes_gcm" "state" {
      keys = key_provider.pbkdf2.state
    }

    state {
      method   = method.aes_gcm.state
      enforced = true
    }

    plan {
      method   = method.aes_gcm.state
      enforced = true
    }
  }
}
