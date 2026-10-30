/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include "imscope_discovery.h"

#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <spdlog/spdlog.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <unistd.h>

#include <atomic>
#include <chrono>
#include <cstring>
#include <sstream>
#include <thread>

std::string imscope_get_primary_ip() {
  int sock = socket(AF_INET, SOCK_DGRAM, 0);
  if (sock < 0) {
    return "127.0.0.1";
  }

  sockaddr_in target{};
  target.sin_family = AF_INET;
  target.sin_addr.s_addr = inet_addr("8.8.8.8");
  target.sin_port = htons(53);

  if (connect(sock, reinterpret_cast<const sockaddr*>(&target),
              sizeof(target)) == 0) {
    sockaddr_in local{};
    socklen_t len = sizeof(local);
    if (getsockname(sock, reinterpret_cast<sockaddr*>(&local), &len) == 0) {
      char ip_buf[INET_ADDRSTRLEN];
      if (inet_ntop(AF_INET, &local.sin_addr, ip_buf, sizeof(ip_buf))) {
        close(sock);
        return std::string(ip_buf);
      }
    }
  }

  close(sock);
  return "127.0.0.1";
}

struct DiscoveryServer {
  std::string name;
  std::string announce_url;
  int sock = -1;
  int wake_fd = -1;
  std::atomic<bool> running{true};
  std::thread thread;

  ~DiscoveryServer() {
    running.store(false);
    if (wake_fd >= 0) {
      uint64_t val = 1;
      write(wake_fd, &val, sizeof(val));
    }
    if (thread.joinable()) {
      thread.join();
    }
    if (sock >= 0) {
      close(sock);
      sock = -1;
    }
    if (wake_fd >= 0) {
      close(wake_fd);
      wake_fd = -1;
    }
  }
};

void* imscope_discovery_server_start(const std::string& name,
                                     const std::string& announce_url) {
  auto server = std::make_unique<DiscoveryServer>();
  server->name = name;
  server->announce_url = announce_url;

  int fd = socket(AF_INET, SOCK_DGRAM, 0);
  if (fd < 0) {
    spdlog::warn("imscope_discovery: failed to create UDP socket: {}",
                 strerror(errno));
    return nullptr;
  }

  int reuse = 1;
  setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
#ifdef SO_REUSEPORT
  setsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &reuse, sizeof(reuse));
#endif

  unsigned char loop = 1;
  setsockopt(fd, IPPROTO_IP, IP_MULTICAST_LOOP, &loop, sizeof(loop));

  unsigned char ttl = 2;
  setsockopt(fd, IPPROTO_IP, IP_MULTICAST_TTL, &ttl, sizeof(ttl));

  sockaddr_in bind_addr{};
  bind_addr.sin_family = AF_INET;
  bind_addr.sin_addr.s_addr = htonl(INADDR_ANY);
  bind_addr.sin_port = htons(IMSCOPE_DISCOVERY_MCAST_PORT);

  if (bind(fd, reinterpret_cast<const sockaddr*>(&bind_addr),
           sizeof(bind_addr)) < 0) {
    spdlog::warn("imscope_discovery: failed to bind UDP port {}: {}",
                 IMSCOPE_DISCOVERY_MCAST_PORT, strerror(errno));
    close(fd);
    return nullptr;
  }

  ip_mreq mreq{};
  mreq.imr_multiaddr.s_addr = inet_addr(IMSCOPE_DISCOVERY_MCAST_ADDR);
  mreq.imr_interface.s_addr = htonl(INADDR_ANY);
  if (setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, &mreq, sizeof(mreq)) < 0) {
    spdlog::warn("imscope_discovery: failed to join multicast group {}: {}",
                 IMSCOPE_DISCOVERY_MCAST_ADDR, strerror(errno));
  }

  server->sock = fd;
  server->wake_fd = eventfd(0, EFD_NONBLOCK);

  server->thread = std::thread([s = server.get()]() {
    sockaddr_in mcast_dest{};
    mcast_dest.sin_family = AF_INET;
    mcast_dest.sin_addr.s_addr = inet_addr(IMSCOPE_DISCOVERY_MCAST_ADDR);
    mcast_dest.sin_port = htons(IMSCOPE_DISCOVERY_MCAST_PORT);

    std::string beacon_payload =
        "IMSCOPE_BEACON " + s->name + " " + s->announce_url;

    auto last_beacon = std::chrono::steady_clock::now() - std::chrono::seconds(1);

    while (s->running.load(std::memory_order_relaxed)) {
      auto now = std::chrono::steady_clock::now();
      if (std::chrono::duration_cast<std::chrono::milliseconds>(now - last_beacon).count() >= 500) {
        // Send periodic multicast beacon
        sendto(s->sock, beacon_payload.c_str(), beacon_payload.size(), 0,
               reinterpret_cast<const sockaddr*>(&mcast_dest),
               sizeof(mcast_dest));
        last_beacon = now;
      }

      pollfd pfds[2]{};
      pfds[0].fd = s->sock;
      pfds[0].events = POLLIN;
      pfds[1].fd = s->wake_fd;
      pfds[1].events = POLLIN;

      int ret = poll(pfds, 2, 250);
      if (ret > 0) {
        if (pfds[1].revents & POLLIN) {
          // Woken up to exit
          break;
        }
        if (pfds[0].revents & POLLIN) {
          char buf[512];
          sockaddr_in sender{};
          socklen_t sender_len = sizeof(sender);
          ssize_t n = recvfrom(s->sock, buf, sizeof(buf) - 1, 0,
                               reinterpret_cast<sockaddr*>(&sender), &sender_len);
          if (n > 0) {
            buf[n] = '\0';
            std::string msg(buf);
            std::istringstream iss(msg);
            std::string cmd, query_name;
            if (iss >> cmd >> query_name) {
              if (cmd == "IMSCOPE_DISCOVER" &&
                  (query_name == s->name || query_name == "*")) {
                // Unicast reply directly to the querier
                sendto(s->sock, beacon_payload.c_str(), beacon_payload.size(), 0,
                       reinterpret_cast<const sockaddr*>(&sender),
                       sender_len);
              }
            }
          }
        }
      }
    }
  });

  return server.release();
}

void imscope_discovery_server_stop(void* handle) {
  if (handle) {
    auto server = static_cast<DiscoveryServer*>(handle);
    delete server;
  }
}

std::string imscope_discovery_client_find(const std::string& name,
                                         int timeout_ms) {
  int fd = socket(AF_INET, SOCK_DGRAM, 0);
  if (fd < 0) {
    return "";
  }

  int reuse = 1;
  setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
#ifdef SO_REUSEPORT
  setsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &reuse, sizeof(reuse));
#endif

  unsigned char loop = 1;
  setsockopt(fd, IPPROTO_IP, IP_MULTICAST_LOOP, &loop, sizeof(loop));

  sockaddr_in bind_addr{};
  bind_addr.sin_family = AF_INET;
  bind_addr.sin_addr.s_addr = htonl(INADDR_ANY);
  bind_addr.sin_port = 0;  // Ephemeral port

  if (bind(fd, reinterpret_cast<const sockaddr*>(&bind_addr),
           sizeof(bind_addr)) < 0) {
    close(fd);
    return "";
  }

  ip_mreq mreq{};
  mreq.imr_multiaddr.s_addr = inet_addr(IMSCOPE_DISCOVERY_MCAST_ADDR);
  mreq.imr_interface.s_addr = htonl(INADDR_ANY);
  setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, &mreq, sizeof(mreq));

  // Multicast destination
  sockaddr_in mcast_dest{};
  mcast_dest.sin_family = AF_INET;
  mcast_dest.sin_addr.s_addr = inet_addr(IMSCOPE_DISCOVERY_MCAST_ADDR);
  mcast_dest.sin_port = htons(IMSCOPE_DISCOVERY_MCAST_PORT);

  // Unicast loopback destination as fallback
  sockaddr_in loopback_dest{};
  loopback_dest.sin_family = AF_INET;
  loopback_dest.sin_addr.s_addr = inet_addr("127.0.0.1");
  loopback_dest.sin_port = htons(IMSCOPE_DISCOVERY_MCAST_PORT);

  std::string query = "IMSCOPE_DISCOVER " + name;

  // Send discovery query over multicast AND loopback
  sendto(fd, query.c_str(), query.size(), 0,
         reinterpret_cast<const sockaddr*>(&mcast_dest), sizeof(mcast_dest));
  sendto(fd, query.c_str(), query.size(), 0,
         reinterpret_cast<const sockaddr*>(&loopback_dest), sizeof(loopback_dest));

  auto deadline = std::chrono::steady_clock::now() +
                  std::chrono::milliseconds(timeout_ms);

  while (true) {
    auto now = std::chrono::steady_clock::now();
    if (now >= deadline) {
      break;
    }
    int remain_ms = std::chrono::duration_cast<std::chrono::milliseconds>(
                        deadline - now)
                        .count();

    pollfd pfd{};
    pfd.fd = fd;
    pfd.events = POLLIN;

    int ret = poll(&pfd, 1, std::min(remain_ms, 200));
    if (ret > 0 && (pfd.revents & POLLIN)) {
      char buf[512];
      ssize_t n = recv(fd, buf, sizeof(buf) - 1, 0);
      if (n > 0) {
        buf[n] = '\0';
        std::istringstream iss(buf);
        std::string cmd, found_name, url;
        if (iss >> cmd >> found_name >> url) {
          if (cmd == "IMSCOPE_BEACON" &&
              (found_name == name || name.empty() || name == "*")) {
            close(fd);
            return url;
          }
        }
      }
    } else if (ret == 0) {
      // Re-send query if still waiting
      sendto(fd, query.c_str(), query.size(), 0,
             reinterpret_cast<const sockaddr*>(&mcast_dest), sizeof(mcast_dest));
      sendto(fd, query.c_str(), query.size(), 0,
             reinterpret_cast<const sockaddr*>(&loopback_dest), sizeof(loopback_dest));
    }
  }

  close(fd);
  return "";
}
