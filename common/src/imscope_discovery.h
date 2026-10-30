/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#pragma once

#include <string>
#include <vector>

constexpr const char* IMSCOPE_DISCOVERY_MCAST_ADDR = "239.255.77.77";
constexpr int IMSCOPE_DISCOVERY_MCAST_PORT = 15555;

// Starts the producer discovery beacon in a background thread.
void* imscope_discovery_server_start(const std::string& name,
                                     const std::string& announce_url);

// Stops the discovery server and frees resources.
void imscope_discovery_server_stop(void* handle);

// Queries for a producer by name over UDP multicast (and optional unicast host).
// Returns the announce_url if found within timeout_ms, or empty string.
std::string imscope_discovery_client_find(const std::string& name,
                                         const std::string& host = "",
                                         int timeout_ms = 1000);

// Resolves the primary local IP address.
std::string imscope_get_primary_ip();
