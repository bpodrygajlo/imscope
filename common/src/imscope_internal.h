/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#pragma once

#include <zenoh.hxx>
#include <string>

#include "imscope_common.h"

// Opens a Zenoh session, optionally connecting to an explicit `locator`
// (e.g. "tcp/127.0.0.1:7447"). A null or empty `locator` relies on Zenoh's
// default scouting/discovery instead, mirroring the Rust producer/consumer's
// `Option<&str>::None` convention (see `producer/src/producer.rs::new` and
// `src/consumer.rs::open_session`). Shared by both the producer and the
// consumer. Throws `zenoh::ZException` if the session fails to open.
zenoh::Session open_zenoh_session(const char* locator);

// Test-only hooks used by common/test/integration_test.cpp so it can pair a
// producer and a consumer session over an explicit, same-machine TCP
// endpoint instead of relying on Zenoh's default multicast scouting (which
// may be slow or unavailable in sandboxed/CI environments). Every real
// caller of `imscope_init_producer`/`ImscopeConsumer::connect` leaves these
// at their default (empty/false) values, so production behavior is
// unaffected.
//
// `g_test_extra_listen_locator`, when non-empty, makes the *next*
// `open_zenoh_session()` call also listen on that endpoint (in addition to
// whatever `locator` it was given to connect to); the test sets it
// immediately before calling `imscope_init_producer()` for the producer
// side of a pairing and a matching `connect`-locator on the consumer side.
// `g_test_disable_multicast_scouting`, when true, disables multicast
// scouting for every session opened while it is set.
extern std::string g_test_extra_listen_locator;
extern bool g_test_disable_multicast_scouting;

void print_announce_response(announce_response_t* msg);
