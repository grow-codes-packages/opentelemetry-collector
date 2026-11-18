# TypeScript example

```sh
(cd ../../typescript && npm install && npm run build)
npm install
GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:<port> GROW_OPENTELEMETRY_COLLECTOR_TOKEN=... npm start
```

Replace the `file:` dependency in `package.json` with a version (`npm install @grow-codes/opentelemetry-collector`)
to run it against the published package.
