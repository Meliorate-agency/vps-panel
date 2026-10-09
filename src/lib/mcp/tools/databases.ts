import "server-only";
import { agent } from "@/lib/agent";
import { optionalString, requireString, type Tool } from "../types";

export const TOOLS: Tool[] = [
  {
    page: "Databases",
    name: "db.list_users",
    description: "List MariaDB users (user@host pairs).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.databases.listUsers(),
  },
  {
    page: "Databases",
    name: "db.list_databases",
    description:
      "List MariaDB databases (excluding system schemas). Each row includes users: the user@host pairs holding db-level grants on it (global ON *.* grants are not listed per-database).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    call: async () => agent.databases.listDatabases(),
  },
  {
    page: "Databases",
    name: "db.create_user",
    description:
      "Create a MariaDB user. Password is generated when omitted and returned in initial_password — copy it immediately. host defaults to 'localhost'.",
    inputSchema: {
      type: "object",
      properties: {
        user: { type: "string", description: "MariaDB username." },
        password: { type: "string", description: "Optional password.", sensitive: true },
        host: { type: "string", description: "Host the user may connect from. Default 'localhost'.", default: "localhost" },
      },
      required: ["user"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.databases.createUser({
        user: requireString(args, "user"),
        password: optionalString(args, "password"),
        host: optionalString(args, "host"),
      }),
  },
  {
    page: "Databases",
    name: "db.create_database",
    description: "Create a MariaDB database (CREATE DATABASE IF NOT EXISTS).",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string", description: "Database name." } },
      required: ["name"],
      additionalProperties: false,
    },
    call: async (args) => agent.databases.createDatabase(requireString(args, "name")),
  },
  {
    page: "Databases",
    name: "db.grant",
    description: "Grant privileges on a database to a user. Default privileges 'ALL PRIVILEGES'.",
    inputSchema: {
      type: "object",
      properties: {
        user: { type: "string", description: "MariaDB user." },
        host: { type: "string", description: "User's host. Default 'localhost'.", default: "localhost" },
        database: { type: "string", description: "Database to grant on." },
        privileges: {
          type: "string",
          description: "Privilege list. Default 'ALL PRIVILEGES'.",
          default: "ALL PRIVILEGES",
        },
      },
      required: ["user", "database"],
      additionalProperties: false,
    },
    call: async (args) =>
      agent.databases.grant({
        user: requireString(args, "user"),
        host: optionalString(args, "host"),
        database: requireString(args, "database"),
        privileges: optionalString(args, "privileges"),
      }),
  },
];
