default:
    @just --list

compounds_uniq lang="sme":
    cargo run --release -- {{lang}} | sort | uniq > compounds_uniq_
