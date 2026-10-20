fix: A `tsc: build - <file>` task detected from a tsconfig is waited for until its build finishes when it runs as a `preLaunchTask`, as any task that is not a background task is.
