---
name: start-dev
description: Start the Vite dev server in the background and wait until it accepts connections on :5173.
allowed-tools: Bash
user-invocable: true
---

Start vite dev in the background and block until it's listening on
:5173. Required before running visual-diff tests.

```bash
cd ts
pnpm dev > /tmp/vite.log 2>&1 &
disown

cd ..

for i in 1 2 3 4 5 6 7 8 9 10; do
    if curl -sI http://localhost:5173/ 2>/dev/null | head -1 | grep -q "200"; then
        echo "vite ready (iter=$i)"
        echo ""
        echo "headers:"
        curl -sI http://localhost:5173/ | grep -iE "cross-origin|content-type"
        break
    fi
    sleep 1
done
```

Stop with `/stop-dev` or `pgrep -f vite | xargs kill`.

If the server doesn't come up, check `/tmp/vite.log` for the failure
reason (port already in use, missing node_modules, etc).
