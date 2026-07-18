# How to Push to Binary Caches

Xi includes a persistent cache push system with async background pushes,
automatic retry, and queue management.

## Push after a build

Push to an S3 cache:

```sh
xi os switch --push-to "s3://my-cache?region=eu-west-1" --sign-key /path/to/key
```

Push using an external command (e.g. Cachix):

```sh
xi os switch --push-cmd cachix push mycache
```

## Async push

Push in the background so the switch completes immediately:

```sh
xi os switch --push-to "s3://..." --sign-key /path/to/key --async-push
```

Or set `XI_CACHE_ASYNC=1` globally.

Failed pushes are automatically enqueued with retry logic.

## Configure in config.toml

```toml
[cache]
async_push = true
queue_max_size = 100
queue_expiry_days = 7

[cache.my-s3]
push_url = "s3://my-bucket?region=eu-west-1"
signing_key = "/path/to/signing-key"

[cache.cachix]
push_command = ["cachix", "push", "mycache"]
```

## Manage the push queue

Check pending pushes:

```sh
xi cache status
```

Retry failed pushes:

```sh
xi cache retry
```

Retry with age limit:

```sh
xi cache retry --max-age-days 3
```

Clear on persistent failures:

```sh
xi cache retry --clear-on-failure
```

Clear the entire queue:

```sh
xi cache clear
```

## Disable pushing

```sh
xi os switch --no-push
```

## Environment variables

- `XI_CACHE_URL` — Default push URL
- `XI_SIGNING_KEY` — Default signing key path
- `XI_CACHE_ASYNC` — Enable async push globally

## Queue storage

The queue is persisted at `$XDG_STATE_HOME/xi/cache/queue.json`. Entries are
deduplicated by (store path, target name) and expire after `queue_expiry_days`.
