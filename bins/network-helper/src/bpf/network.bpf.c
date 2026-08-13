// SPDX-License-Identifier: GPL-2.0-only
#include <linux/types.h>

#define BPF_MAP_TYPE_PERCPU_HASH 5
#define BPF_MAP_TYPE_PERCPU_ARRAY 6
#define BPF_NOEXIST 1

#include <bpf/bpf_helpers.h>

#define MAX_CGROUPS 4096
#define TRAFFIC_COUNTER_OVERFLOW (1ULL << 0)
#define COLLECTOR_MAP_SATURATED (1ULL << 0)

/* The preserved kernel type makes the context access CO-RE relocatable. */
struct __sk_buff {
    __u32 len;
} __attribute__((preserve_access_index));

struct traffic_value {
    __u64 rx_bytes;
    __u64 tx_bytes;
    __u64 flags;
};

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_HASH);
    __uint(max_entries, MAX_CGROUPS);
    __type(key, __u64);
    __type(value, struct traffic_value);
} cgroup_counters SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u64);
} collector_health SEC(".maps");

static __always_inline void mark_map_saturated(void)
{
    __u32 key = 0;
    __u64 *flags = bpf_map_lookup_elem(&collector_health, &key);

    if (flags)
        *flags |= COLLECTOR_MAP_SATURATED;
}

static __always_inline int account(struct __sk_buff *skb, int ingress)
{
    /* Ingress can run in softirq context, so attribute by the skb's socket. */
    __u64 cgroup_id = bpf_skb_cgroup_id(skb);
    struct traffic_value zero = {};
    struct traffic_value *value;
    __u64 bytes = skb->len;

    value = bpf_map_lookup_elem(&cgroup_counters, &cgroup_id);
    if (!value) {
        /* A concurrent CPU may win BPF_NOEXIST; always look up again. */
        bpf_map_update_elem(&cgroup_counters, &cgroup_id, &zero, BPF_NOEXIST);
        value = bpf_map_lookup_elem(&cgroup_counters, &cgroup_id);
        if (!value) {
            mark_map_saturated();
            return 1;
        }
    }

    if (ingress) {
        if (value->rx_bytes > ~0ULL - bytes)
            value->flags |= TRAFFIC_COUNTER_OVERFLOW;
        else
            value->rx_bytes += bytes;
    } else {
        if (value->tx_bytes > ~0ULL - bytes)
            value->flags |= TRAFFIC_COUNTER_OVERFLOW;
        else
            value->tx_bytes += bytes;
    }
    return 1;
}

SEC("cgroup_skb/ingress")
int count_ingress(struct __sk_buff *skb)
{
    return account(skb, 1);
}

SEC("cgroup_skb/egress")
int count_egress(struct __sk_buff *skb)
{
    return account(skb, 0);
}

char LICENSE[] SEC("license") = "GPL";
