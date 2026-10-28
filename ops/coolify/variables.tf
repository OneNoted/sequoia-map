variable "state_passphrase" {
  description = "Passphrase that encrypts the state and saved plans (at least 16 characters)."
  type        = string
  sensitive   = true
}

variable "project_uuid" {
  description = "UUID of the Coolify project holding the application."
  type        = string
}

variable "server_uuid" {
  description = "UUID of the Coolify server the application runs on."
  type        = string
}

variable "docker_compose_location" {
  description = "Compose file Coolify deploys, relative to the repository root."
  type        = string
  default     = "/compose.yaml"
}
