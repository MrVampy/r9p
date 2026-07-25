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
      auth: "p9any:noise-ik@vault",
      pid: 42,
      protocol: "9P2000",
      msize: 65_536,
      localRootLabel: "example",
      namespaceMountPaths: ["/venues/example/demo/sensor"],
      sessionEndpoint: {
        endpointBind: "192.168.0.20:4568",
        aname: "/",
        auth: "p9any:noise-ik@example",
      },
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
      "auth\tp9any:noise-ik@vault\n" +
      "pid\t42\n" +
      "protocol\t9P2000\n" +
      "msize\t65536\n" +
      "local_root_label\texample\n" +
      "namespace_mount_paths\t/venues/example/demo/sensor\n" +
      "session_endpoint_bind\t192.168.0.20:4568\n" +
      "session_aname\t/\n" +
      "session_auth\tp9any:noise-ik@example\n" +
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

Deno.test("renderExportDescriptor validates the session endpoint boundary", () => {
  assertThrows(() =>
    renderExportDescriptor({
      endpointBind: "127.0.0.1:4567",
      aname: "/",
      uname: "codex",
      exportedRoot: "/",
      transportClass: "tcp",
      mode: "ro",
      auth: "none",
      pid: 42,
      protocol: "9P2000",
      msize: 65_536,
      sessionEndpoint: {
        endpointBind: "192.168.0.20:4568",
        aname: "/",
        auth: "none",
      },
    })
  );
});

Deno.test("renderExportDescriptor validates the p9any provider and domain", () => {
  for (const auth of ["p9any:dp9ik@vault", "p9any:noise-ik@vault/domain"]) {
    assertThrows(() =>
      renderExportDescriptor({
        endpointBind: "192.168.0.20:4567",
        aname: "/",
        uname: "codex",
        exportedRoot: "/",
        transportClass: "tcp",
        mode: "ro",
        auth,
        pid: 42,
        protocol: "9P2000",
        msize: 65_536,
      })
    );
  }
});
