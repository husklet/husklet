use super::*;
use naga::{ImageClass, ImageQuery, Scalar, ScalarKind, VectorSize};

pub(super) struct Load {
    pub pointers: Vec<Handle<Expression>>,
    pub selector: Handle<Expression>,
    pub coordinate: Handle<Expression>,
    pub array_index: Option<Handle<Expression>>,
    pub sample: Option<Handle<Expression>>,
    pub level: Option<Handle<Expression>>,
    pub image_ty: Handle<Type>,
}

impl FunctionLowering<'_> {
    pub(super) fn image_load(
        &self,
        load: Load,
        span: Span,
        expressions: &mut Arena<Expression>,
    ) -> Result<Handle<Expression>> {
        let TypeInner::Image {
            class: ImageClass::Storage { format, .. },
            ..
        } = self.types[load.image_ty].inner
        else {
            return Err(GpuError::Unsupported(
                "only storage-image descriptor loads are scalarized",
            ));
        };
        let scalar = Scalar::from(format);
        let ty = self.vector_type(VectorSize::Quad, scalar)?;
        self.select_image_value(
            &load.pointers,
            load.selector,
            ty,
            span,
            expressions,
            |image| Expression::ImageLoad {
                image,
                coordinate: load.coordinate,
                array_index: load.array_index,
                sample: load.sample,
                level: load.level,
            },
        )
    }

    pub(super) fn image_query(
        &self,
        pointers: &[Handle<Expression>],
        selector: Handle<Expression>,
        query: &ImageQuery,
        image_ty: Handle<Type>,
        span: Span,
        expressions: &mut Arena<Expression>,
    ) -> Result<Handle<Expression>> {
        let TypeInner::Image { dim, arrayed, .. } = self.types[image_ty].inner else {
            return Err(GpuError::Invalid(
                "storage-image descriptor has a non-image type",
            ));
        };
        let scalar = Scalar {
            kind: ScalarKind::Uint,
            width: 4,
        };
        let components = match query {
            ImageQuery::Size { .. } => {
                let coordinates = match dim {
                    naga::ImageDimension::Buffer => 1,
                    naga::ImageDimension::D1 => 1,
                    naga::ImageDimension::D2 | naga::ImageDimension::Cube => 2,
                    naga::ImageDimension::D3 => 3,
                };
                coordinates + usize::from(arrayed)
            }
            ImageQuery::NumLevels | ImageQuery::NumLayers | ImageQuery::NumSamples => 1,
        };
        let ty = match components {
            1 => self.scalar_type(scalar)?,
            2 => self.vector_type(VectorSize::Bi, scalar)?,
            3 => self.vector_type(VectorSize::Tri, scalar)?,
            _ => {
                return Err(GpuError::Unsupported(
                    "storage-image query result shape is unsupported",
                ))
            }
        };
        self.select_image_value(pointers, selector, ty, span, expressions, |image| {
            Expression::ImageQuery {
                image,
                query: *query,
            }
        })
    }

    fn select_image_value(
        &self,
        pointers: &[Handle<Expression>],
        selector: Handle<Expression>,
        ty: Handle<Type>,
        span: Span,
        expressions: &mut Arena<Expression>,
        expression: impl Fn(Handle<Expression>) -> Expression,
    ) -> Result<Handle<Expression>> {
        let mut selected = expressions.append(Expression::ZeroValue(ty), span);
        for (element, image) in pointers.iter().copied().enumerate() {
            let value = expressions.append(expression(image), span);
            let literal =
                expressions.append(Expression::Literal(Literal::U32(element as u32)), span);
            let condition = expressions.append(
                Expression::Binary {
                    op: BinaryOperator::Equal,
                    left: selector,
                    right: literal,
                },
                span,
            );
            selected = expressions.append(
                Expression::Select {
                    condition,
                    accept: value,
                    reject: selected,
                },
                span,
            );
        }
        Ok(selected)
    }

    fn scalar_type(&self, scalar: Scalar) -> Result<Handle<Type>> {
        self.types
            .iter()
            .find_map(|(handle, ty)| {
                matches!(ty.inner, TypeInner::Scalar(candidate) if candidate == scalar)
                    .then_some(handle)
            })
            .ok_or(GpuError::Invalid(
                "storage-image scalar result type is absent",
            ))
    }

    fn vector_type(&self, size: VectorSize, scalar: Scalar) -> Result<Handle<Type>> {
        self.types
            .iter()
            .find_map(|(handle, ty)| {
                matches!(
                    ty.inner,
                    TypeInner::Vector {
                        size: candidate_size,
                        scalar: candidate_scalar,
                    } if candidate_size == size && candidate_scalar == scalar
                )
                .then_some(handle)
            })
            .ok_or(GpuError::Invalid(
                "storage-image vector result type is absent",
            ))
    }
}
