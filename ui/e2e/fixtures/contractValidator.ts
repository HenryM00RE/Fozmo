import Ajv, { type ErrorObject, type ValidateFunction } from 'ajv';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

type JsonRecord = Record<string, unknown>;

type EndpointContract = {
  method: string;
  path: string;
  response_schema: string;
  request_schema?: string;
  loose_reason?: string;
};

type ContractSchema = {
  $schema?: string;
  definitions?: JsonRecord;
  'x-fozmo-endpoints'?: EndpointContract[];
};

const schemaPath = resolve(
  dirname(fileURLToPath(import.meta.url)),
  '../../src/shared/generated/api-contract.schema.json'
);
const contractSchema = JSON.parse(readFileSync(schemaPath, 'utf8')) as ContractSchema;
const endpointContracts = contractSchema['x-fozmo-endpoints'] || [];
const ajv = new Ajv({ allErrors: true, strict: false, validateFormats: false });
const validators = new Map<string, ValidateFunction>();

export function validateContractResponse(method: string, path: string, body: unknown) {
  const contract = matchingContract(method, path);
  if (!contract || contract.loose_reason) return;

  const validate = validatorFor(contract.response_schema);
  if (validate(body)) return;

  throw new Error(
    `${method} ${path} failed ${contract.response_schema} contract validation: ${formatErrors(
      validate.errors
    )}`
  );
}

function matchingContract(method: string, path: string) {
  const normalizedMethod = method.toUpperCase();
  return endpointContracts.find(
    (contract) =>
      contract.method.toUpperCase() === normalizedMethod && pathMatchesTemplate(contract.path, path)
  );
}

function pathMatchesTemplate(template: string, path: string) {
  if (template === path) return true;
  const templateParts = template.split('/').filter(Boolean);
  const pathParts = path.split('/').filter(Boolean);

  for (let index = 0; index < templateParts.length; index += 1) {
    const templatePart = templateParts[index];
    const pathPart = pathParts[index];
    if (templatePart === '*') return true;
    if (pathPart === undefined) return false;
    if (templatePart.startsWith(':')) continue;
    if (templatePart !== pathPart) return false;
  }

  return templateParts.length === pathParts.length;
}

function validatorFor(responseSchema: string) {
  const cached = validators.get(responseSchema);
  if (cached) return cached;
  const validate = ajv.compile(schemaForResponse(responseSchema));
  validators.set(responseSchema, validate);
  return validate;
}

function schemaForResponse(responseSchema: string): JsonRecord {
  const definitions = contractSchema.definitions || {};
  if (responseSchema === 'JsonRecord') {
    return { type: 'object', additionalProperties: true };
  }
  if (responseSchema.endsWith('[]')) {
    const itemSchema = responseSchema.slice(0, -2);
    return {
      $schema: contractSchema.$schema,
      definitions,
      type: 'array',
      items: definitionRef(itemSchema, definitions)
    };
  }
  return {
    $schema: contractSchema.$schema,
    definitions,
    allOf: [definitionRef(responseSchema, definitions)]
  };
}

function definitionRef(schemaName: string, definitions: JsonRecord) {
  if (!definitions[schemaName]) {
    throw new Error(`Missing API contract schema definition: ${schemaName}`);
  }
  return { $ref: `#/definitions/${schemaName}` };
}

function formatErrors(errors: ErrorObject[] | null | undefined) {
  if (!errors?.length) return 'unknown schema error';
  return errors
    .slice(0, 5)
    .map((error) => {
      const location = error.instancePath || '/';
      return `${location} ${error.message || 'is invalid'}`;
    })
    .join('; ');
}
