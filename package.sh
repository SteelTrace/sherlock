mkdir -p dist/sherlock
cp target/release/sherlock dist/sherlock/
tar -czf sherlock.tar.gz -C dist sherlock
