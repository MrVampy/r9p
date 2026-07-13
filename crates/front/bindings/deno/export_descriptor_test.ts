import { assertEquals, assertThrows } from "jsr:@std/assert@^1.0.0";
import { renderExportDescriptor } from "./export_descriptor.ts";

Deno.test("renderExportDescriptor emits the canonical field order", () => {
  assertEquals(
    renderExportDescriptor({
      endpointBind: "192.168.0.20:4567",
      aname: "/",
      uname: "codex",
      exportedRoot: "/sensor",
      transportClass: "tcp",
      mode: "ro",
      auth: "wg:vault-runtime-lan",
      pid: 42,
      protocol: "9P2000",
      msize: 65_536,
      localRootLabel: "example",
      namespaceMountPaths: ["/venues/example/demo/sensor"],
      extraFields: {
        service_unit: "example.service",
        host_firewall_admission: "tcp:192.168.0.20:4567",
      },
    }),
    "format\tr9p-export.v1\n" +
      "endpoint_bind\t192.168.0.20:4567\n" +
      "aname\t/\n" +
      "uname\tcodex\n" +
      "exported_root\t/sensor\n" +
      "transport_class\ttcp\n" +
      "mode\tro\n" +
      "auth\twg:vault-runtime-lan\n" +
      "pid\t42\n" +
      "protocol\t9P2000\n" +
      "msize\t65536\n" +
      "local_root_label\texample\n" +
      "namespace_mount_paths\t/venues/example/demo/sensor\n" +
      "host_firewall_admission\ttcp:192.168.0.20:4567\n" +
      "service_unit\texample.service\n",
  );
});

Deno.test("renderExportDescriptor rejects invalid authority boundaries", () => {
  assertThrows(() =>
    renderExportDescriptor({
      endpointBind: "192.168.0.20:4567",
      aname: "/",
      uname: "codex",
      exportedRoot: "/",
      transportClass: "tcp",
      mode: "ro",
      auth: "none",
      pid: 42,
      protocol: "9P2000",
      msize: 65_536,
    })
  );
});
