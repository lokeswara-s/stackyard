import type { Service } from "./types";

/** Docker-backed building blocks. "Installing" one pulls the image; adding it to a workspace creates an infra service. */
export interface Plugin {
  id: string;
  name: string;
  category: "Database" | "Cache / Queue" | "Storage" | "Mocks & Tools";
  icon: string;
  description: string;
  image: string;
  port: number;
  containerPort: number;
  dataPath?: string;
  command?: string;
  env: { key: string; value: string }[];
}

export const PLUGINS: Plugin[] = [
  {
    id: "postgres",
    name: "PostgreSQL",
    category: "Database",
    icon: "database",
    description: "Relational database with a persistent named volume.",
    image: "postgres:16-alpine",
    port: 5432,
    containerPort: 5432,
    dataPath: "/var/lib/postgresql/data",
    env: [
      { key: "POSTGRES_USER", value: "postgres" },
      { key: "POSTGRES_PASSWORD", value: "postgres" },
      { key: "POSTGRES_DB", value: "app" },
    ],
  },
  {
    id: "mysql",
    name: "MySQL",
    category: "Database",
    icon: "database",
    description: "MySQL 8 with a persistent named volume.",
    image: "mysql:8",
    port: 3306,
    containerPort: 3306,
    dataPath: "/var/lib/mysql",
    env: [
      { key: "MYSQL_ROOT_PASSWORD", value: "root" },
      { key: "MYSQL_DATABASE", value: "app" },
    ],
  },
  {
    id: "mongo",
    name: "MongoDB",
    category: "Database",
    icon: "database",
    description: "Document database with a persistent named volume.",
    image: "mongo:7",
    port: 27017,
    containerPort: 27017,
    dataPath: "/data/db",
    env: [],
  },
  {
    id: "redis",
    name: "Redis",
    category: "Cache / Queue",
    icon: "memory",
    description: "In-memory cache / session store.",
    image: "redis:7-alpine",
    port: 6379,
    containerPort: 6379,
    dataPath: "/data",
    env: [],
  },
  {
    id: "rabbitmq",
    name: "RabbitMQ",
    category: "Cache / Queue",
    icon: "swap_horiz",
    description: "AMQP broker (management UI on :15672 inside the container).",
    image: "rabbitmq:3-management-alpine",
    port: 5672,
    containerPort: 5672,
    env: [
      { key: "RABBITMQ_DEFAULT_USER", value: "guest" },
      { key: "RABBITMQ_DEFAULT_PASS", value: "guest" },
    ],
  },
  {
    id: "minio",
    name: "MinIO",
    category: "Storage",
    icon: "folder_zip",
    description: "S3-compatible object storage.",
    image: "minio/minio:latest",
    port: 9000,
    containerPort: 9000,
    dataPath: "/data",
    command: "server /data",
    env: [
      { key: "MINIO_ROOT_USER", value: "minio" },
      { key: "MINIO_ROOT_PASSWORD", value: "minio12345" },
    ],
  },
  {
    id: "mailpit",
    name: "Mailpit",
    category: "Mocks & Tools",
    icon: "mail",
    description: "Local SMTP sink that catches outgoing email.",
    image: "axllent/mailpit:latest",
    port: 1025,
    containerPort: 1025,
    env: [],
  },
  {
    id: "wiremock",
    name: "WireMock",
    category: "Mocks & Tools",
    icon: "api",
    description: "HTTP mock server for third-party APIs.",
    image: "wiremock/wiremock:latest",
    port: 8080,
    containerPort: 8080,
    env: [],
  },
];

export function pluginToService(p: Plugin, takenNames: string[]): Service {
  let name = p.id;
  let n = 2;
  while (takenNames.includes(name)) name = `${p.id}-${n++}`;
  return {
    name,
    kind: "infra",
    stack: p.name,
    repoPath: "",
    command: p.command ?? "",
    port: p.port,
    dependsOn: [],
    image: p.image,
    containerPort: p.containerPort,
    dataPath: p.dataPath ?? null,
    env: p.env.map((e) => ({ ...e, secret: /PASSWORD|SECRET|KEY/.test(e.key), profile: null })),
  };
}
