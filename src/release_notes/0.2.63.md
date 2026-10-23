fix: Terminal secret redaction now masks the AWS secret access key and session token (in `~/.aws/credentials`, `env`, `export` lines, STS JSON and the `aws configure` prompt), not just the access key ID.
feature: Terminal secret redaction also masks GitLab, Stripe and Google API keys and the password in `scheme://user:password@host` URLs.
