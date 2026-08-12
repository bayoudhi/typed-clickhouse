import ts, { factory } from "typescript";
import { isLibraryFile, type TransformContext } from "../compilerPluginHelper";
import { toColumns } from "../dataModels/typeConvert";
import type { Column } from "../dataModels/dataModelTypes";
import {
  generateValidateFunction,
  generateIsFunction,
  generateAssertFunction,
  generateJsonSchemas,
  generateInsertValidateFunction,
  generateInsertIsFunction,
  generateInsertAssertFunction,
  type InsertColumnSets,
} from "../typiaDirectIntegration";

const typesToArgsLength = new Map([
  ["OlapTable", 2],
  ["MaterializedView", 1],
]);

export const isNewTchResourceWithTypeParam = (
  node: ts.Node,
  checker: ts.TypeChecker,
): node is ts.NewExpression => {
  if (!ts.isNewExpression(node)) {
    return false;
  }

  const declaration: ts.Declaration | undefined =
    checker.getResolvedSignature(node)?.declaration;

  if (!declaration || !isLibraryFile(declaration.getSourceFile())) {
    return false;
  }
  const sym = checker.getSymbolAtLocation(node.expression);
  const typeName = sym?.name ?? "";
  if (!typesToArgsLength.has(typeName)) {
    return false;
  }

  // Require arguments to be present
  if (!node.arguments) {
    return false;
  }

  const expectedArgLength = typesToArgsLength.get(typeName)!;
  const actualArgLength = node.arguments.length;

  // Check if this is an untransformed tch resource
  // Transformed resources have more arguments (schema, columns, validators, etc.)
  const isUntransformed =
    actualArgLength === expectedArgLength - 1 || // name only
    actualArgLength === expectedArgLength; // name + config

  return isUntransformed && node.typeArguments?.length === 1;
};

export const parseAsAny = (s: string) =>
  factory.createAsExpression(
    factory.createCallExpression(
      factory.createPropertyAccessExpression(
        factory.createIdentifier("JSON"),
        factory.createIdentifier("parse"),
      ),
      undefined,
      [factory.createStringLiteral(s)],
    ),
    factory.createKeywordTypeNode(ts.SyntaxKind.AnyKeyword),
  );

export const transformNewTchResource = (
  node: ts.NewExpression,
  checker: ts.TypeChecker,
  ctx: TransformContext,
): ts.Node => {
  const typeNode = node.typeArguments![0];

  const typeAtLocation = checker.getTypeAtLocation(typeNode);

  // Get the typia context for direct code generation
  const typiaCtx = ctx.typiaContext;

  // Neither OlapTable nor MaterializedView allow index signatures: both
  // require a fixed ClickHouse schema.
  const columns: Column[] = toColumns(typeAtLocation, checker, {
    allowIndexSignatures: false,
  });
  const internalArguments: ts.Expression[] = [
    generateJsonSchemas(typiaCtx, typeAtLocation),
    parseAsAny(JSON.stringify(columns)),
  ];

  const resourceName = checker.getSymbolAtLocation(node.expression)!.name;

  const argLength = typesToArgsLength.get(resourceName)!;
  const needsExtraArg = node.arguments!.length === argLength - 1; // provide empty config if undefined

  let updatedArgs = [
    ...node.arguments!,
    ...(needsExtraArg ?
      [factory.createObjectLiteralExpression([], false)]
    : []),
    ...internalArguments,
  ];

  // For OlapTable, also inject typia validation functions
  if (resourceName === "OlapTable") {
    // Create a single TypiaValidators object with all three validation functions
    // using direct typia code generation (uses shared typiaCtx for imports)
    const validatorsObject = factory.createObjectLiteralExpression(
      [
        factory.createPropertyAssignment(
          factory.createIdentifier("validate"),
          wrapValidateFunction(
            generateValidateFunction(typiaCtx, typeAtLocation),
          ),
        ),
        factory.createPropertyAssignment(
          factory.createIdentifier("assert"),
          generateAssertFunction(typiaCtx, typeAtLocation),
        ),
        factory.createPropertyAssignment(
          factory.createIdentifier("is"),
          generateIsFunction(typiaCtx, typeAtLocation),
        ),
      ],
      true,
    );

    updatedArgs = [...updatedArgs, validatorsObject];

    // For OlapTable, also generate insert validators with Insertable<T> semantics
    // (excludes ALIAS/MATERIALIZED fields, makes DEFAULT fields optional).
    // Uses metadata-patching: typia analyzes the original type T, but
    // MetadataFactory.analyze is intercepted to strip computed columns.
    if (resourceName === "OlapTable" && columns) {
      const insertColumnSets: InsertColumnSets = {
        computed: new Set(
          columns
            .filter((c) => c.alias != null || c.materialized != null)
            .map((c) => c.name),
        ),
        defaults: new Set(
          columns.filter((c) => c.default != null).map((c) => c.name),
        ),
      };

      const insertValidatorsObject = factory.createObjectLiteralExpression(
        [
          factory.createPropertyAssignment(
            factory.createIdentifier("validate"),
            wrapValidateFunction(
              generateInsertValidateFunction(
                typiaCtx,
                typeAtLocation,
                insertColumnSets,
              ),
            ),
          ),
          factory.createPropertyAssignment(
            factory.createIdentifier("assert"),
            generateInsertAssertFunction(
              typiaCtx,
              typeAtLocation,
              insertColumnSets,
            ),
          ),
          factory.createPropertyAssignment(
            factory.createIdentifier("is"),
            generateInsertIsFunction(
              typiaCtx,
              typeAtLocation,
              insertColumnSets,
            ),
          ),
        ],
        true,
      );
      updatedArgs = [...updatedArgs, insertValidatorsObject];
    }
  }

  return ts.factory.updateNewExpression(
    node,
    node.expression,
    node.typeArguments,
    updatedArgs,
  );
};

/**
 * Wraps a typia validate function to match our expected interface
 * Transforms typia's IValidation result to our { success, data, errors } format
 */
const wrapValidateFunction = (validateFn: ts.Expression): ts.Expression => {
  // (data: unknown) => {
  //   const result = validateFn(data);
  //   return {
  //     success: result.success,
  //     data: result.success ? result.data : undefined,
  //     errors: result.success ? undefined : result.errors
  //   };
  // }
  return factory.createArrowFunction(
    undefined,
    undefined,
    [
      factory.createParameterDeclaration(
        undefined,
        undefined,
        factory.createIdentifier("data"),
        undefined,
        factory.createKeywordTypeNode(ts.SyntaxKind.UnknownKeyword),
        undefined,
      ),
    ],
    undefined,
    factory.createToken(ts.SyntaxKind.EqualsGreaterThanToken),
    factory.createBlock(
      [
        factory.createVariableStatement(
          undefined,
          factory.createVariableDeclarationList(
            [
              factory.createVariableDeclaration(
                factory.createIdentifier("result"),
                undefined,
                undefined,
                factory.createCallExpression(validateFn, undefined, [
                  factory.createIdentifier("data"),
                ]),
              ),
            ],
            ts.NodeFlags.Const,
          ),
        ),
        factory.createReturnStatement(
          factory.createObjectLiteralExpression(
            [
              factory.createPropertyAssignment(
                factory.createIdentifier("success"),
                factory.createPropertyAccessExpression(
                  factory.createIdentifier("result"),
                  factory.createIdentifier("success"),
                ),
              ),
              factory.createPropertyAssignment(
                factory.createIdentifier("data"),
                factory.createConditionalExpression(
                  factory.createPropertyAccessExpression(
                    factory.createIdentifier("result"),
                    factory.createIdentifier("success"),
                  ),
                  factory.createToken(ts.SyntaxKind.QuestionToken),
                  factory.createPropertyAccessExpression(
                    factory.createIdentifier("result"),
                    factory.createIdentifier("data"),
                  ),
                  factory.createToken(ts.SyntaxKind.ColonToken),
                  factory.createIdentifier("undefined"),
                ),
              ),
              factory.createPropertyAssignment(
                factory.createIdentifier("errors"),
                factory.createConditionalExpression(
                  factory.createPropertyAccessExpression(
                    factory.createIdentifier("result"),
                    factory.createIdentifier("success"),
                  ),
                  factory.createToken(ts.SyntaxKind.QuestionToken),
                  factory.createIdentifier("undefined"),
                  factory.createToken(ts.SyntaxKind.ColonToken),
                  factory.createPropertyAccessExpression(
                    factory.createIdentifier("result"),
                    factory.createIdentifier("errors"),
                  ),
                ),
              ),
            ],
            true,
          ),
        ),
      ],
      true,
    ),
  );
};
