FROM archlinux/base:latest AS sms-irc-compiled

# update OS
RUN pacman -Syu --noconfirm
RUN pacman -S --needed --noconfirm base-devel

# install Rust: download rustup
RUN curl https://sh.rustup.rs -sSf | sh -s -- --default-toolchain stable -y

# The following bits build just the dependencies of the project, without the source-code itself.
# This is so that we can take advantage of docker's caching and not rebuild everything all the damn time :P

WORKDIR /sms-irc
# add all the cargo files, to get deps
ADD ./Cargo.lock /sms-irc/
ADD ./Cargo.toml /sms-irc/
# make dummy src/lib.rs files, to satisfy cargo
RUN /bin/bash -c 'mkdir src; touch src/lib.rs';
# disable incremental compilation (never going to be used, and bloats binaries)
ENV CARGO_INCREMENTAL=0
# build all the dependencies
RUN ~/.cargo/bin/cargo build --release
# remove the dummy src/ lib.rs files
RUN /bin/bash -c 'rm -rf src'

# Now we a build the actual code...

# add the actual code
ADD ./src /sms-irc/src
# add some dependencies
RUN pacman -S --noconfirm postgresql-libs
# build it!
RUN ~/.cargo/bin/cargo build --release

FROM debian:stable-slim AS sms-irc
WORKDIR /sms-irc
RUN apt-get update && apt-get install -y libssl1.1 ca-certificates libpq5
RUN apt-get clean && rm -rf /var/lib/apt/lists/*
COPY --from=sms-irc-compiled /sms-irc/target/release/sms-irc /sms-irc
ENTRYPOINT "/sms-irc/sms-irc"
