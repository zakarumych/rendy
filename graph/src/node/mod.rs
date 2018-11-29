//! Defines node - building block for framegraph.
//!

pub mod render;
pub mod present;

use crate::{
    chain,
    command::{Capability, Family, Submit, Supports, Submission},
    factory::Factory,
    frame::Frames,
    resource::{Buffer, Image},
    BufferId,
    ImageId,
    NodeId,
};

/// Barrier required for node.
///
/// This type is similar to [`gfx_hal::memory::Barrier`]
/// except that it has resource indices instead of references.
///
/// [`gfx_hal::memory::Barrier`]: ../gfx_hal/memory/enum.Barrier.html
#[derive(Clone, Debug)]
pub enum Barrier {
    /// Applies the given access flags to all buffers in the range.
    AllBuffers(std::ops::Range<gfx_hal::buffer::Access>),
    /// Applies the given access flags to all images in the range.
    AllImages(std::ops::Range<gfx_hal::image::Access>),
    /// A memory barrier that defines access to a buffer.
    Buffer {
        /// The access flags controlling the buffer.
        states: std::ops::Range<gfx_hal::buffer::State>,
        /// The buffer the barrier controls.
        target: usize,
    },
    /// A memory barrier that defines access to (a subset of) an image.
    Image {
        /// The access flags controlling the image.
        states: std::ops::Range<gfx_hal::image::State>,
        /// The image the barrier controls.
        target: usize,
        /// A `SubresourceRange` that defines which section of an image the barrier applies to.
        range: gfx_hal::image::SubresourceRange,
    },
}

/// Buffer shared between nodes.
#[derive(Clone, Copy, Debug)]
pub struct NodeBuffer<'a, B: gfx_hal::Backend> {
    /// Buffer reference.
    pub buffer: &'a Buffer<B>,

    /// Buffer state for node.
    pub state: chain::BufferState,
}

/// Image shared between nodes.
#[derive(Clone, Copy, Debug)]
pub struct NodeImage<'a, B: gfx_hal::Backend> {
    /// Image reference.
    pub image: &'a Image<B>,

    /// Image state for node.
    pub state: chain::ImageState,

    /// Specify that node should clear image to this value.
    pub clear: Option<gfx_hal::command::ClearValue>,
}

/// The node is building block of the framegraph.
/// Node defines set of resources and operations to perform over them.
/// Read-only data for operations comes from auxiliary data source `T`.
///
/// # Parameters
///
/// `B` - backend type.
/// `T` - auxiliary data type.
///
pub trait Node<B: gfx_hal::Backend, T: ?Sized>:
    std::fmt::Debug + Sized + Sync + Send + 'static
{
    /// Capability required by node.
    /// Graph will execute this node on command queue that supports this capability level.
    type Capability: Capability;

    /// Description type to instantiate the node.
    type Desc: NodeDesc<B, T, Node = Self>;

    /// Desc creation.
    /// Convenient method if builder implements `Default`.
    fn desc() -> Self::Desc
    where
        Self::Desc: Default,
    {
        Default::default()
    }

    /// Builder creation.
    fn builder(self) -> NodeBuilder<B, T>
    where
        Self::Desc: Default,
    {
        Self::desc().builder()
    }

    /// Record commands required by node.
    /// Returned submits are guaranteed to be submitted within specified frame.
    fn run<'a>(
        &mut self,
        factory: &mut Factory<B>,
        aux: &mut T,
        frames: &'a Frames<B>,
    ) -> Submit<'a, B>;

    /// Dispose of the node.
    /// 
    /// # Safety
    /// 
    /// Must be called after waiting for device idle.
    unsafe fn dispose(self, factory: &mut Factory<B>, aux: &mut T);
}

/// Builder of the node.
/// Implementation of the builder type provide framegraph with static information about node
/// that is used for building the node.
pub trait NodeDesc<B: gfx_hal::Backend, T: ?Sized>: std::fmt::Debug + Sized + 'static {
    /// Node this builder builds.
    type Node: Node<B, T>;

    /// Make node builder.
    fn builder(self) -> NodeBuilder<B, T> {
        NodeBuilder {
            desc: Box::new((self,)),
            buffers: Vec::new(),
            images: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    /// Get set or buffer resources the node uses.
    fn buffers(&self) -> Vec<chain::BufferState> {
        Vec::new()
    }

    /// Get set or image resources the node uses.
    fn images(&self) -> Vec<chain::ImageState> {
        Vec::new()
    }

    /// Build the node.
    ///
    /// # Parameters
    ///
    /// `factory`    - factory instance.
    /// `aux`       - auxiliary data.
    /// `family`    - id of the family this node will be executed on.
    /// `resources` - set of transient resources managed by graph.
    ///               with barriers required for interface resources.
    ///
    fn build<'a>(
        &self,
        factory: &mut Factory<B>,
        aux: &mut T,
        family: gfx_hal::queue::QueueFamilyId,
        buffers: impl IntoIterator<Item = NodeBuffer<'a, B>>,
        images: impl IntoIterator<Item = NodeImage<'a, B>>,
    ) -> Result<Self::Node, failure::Error>;
}

/// Trait-object safe `Node`.
pub trait AnyNode<B: gfx_hal::Backend, T: ?Sized>:
    std::fmt::Debug + Sync + Send
{
    /// Record commands required by node.
    /// Recorded buffers go into `submits`.
    unsafe fn run<'a>(
        &mut self,
        factory: &mut Factory<B>,
        aux: &mut T,
        frames: &Frames<B>,
        qid: chain::QueueId,
        waits: &[(&'a B::Semaphore, gfx_hal::pso::PipelineStage)],
        signals: &[&'a B::Semaphore],
        fence: Option<&B::Fence>,
    );

    /// Dispose of the node.
    /// 
    /// # Safety
    /// 
    /// Must be called after waiting for device idle.
    unsafe fn dispose(self: Box<Self>, factory: &mut Factory<B>, aux: &mut T);
}

impl<B, T, N> AnyNode<B, T> for (N,)
where
    B: gfx_hal::Backend,
    T: ?Sized,
    N: Node<B, T>,
{
    unsafe fn run<'a>(
        &mut self,
        factory: &mut Factory<B>,
        aux: &mut T,
        frames: &Frames<B>,
        qid: chain::QueueId,
        waits: &[(&'a B::Semaphore, gfx_hal::pso::PipelineStage)],
        signals: &[&'a B::Semaphore],
        fence: Option<&B::Fence>,
    ) {
        let submit = Node::run(&mut self.0, factory, aux, frames);
        factory.family_mut(qid.family()).submit(
            qid.index(),
            Some(Submission {
                waits: waits.iter().cloned(),
                signals: signals.iter().cloned(),
                submits: Some(submit),
            }),
            fence,
        )
    }

    unsafe fn dispose(self: Box<Self>, factory: &mut Factory<B>, aux: &mut T) {
        N::dispose(self.0, factory, aux);
    }
}

/// Trait-object safe `NodeDesc`.
pub trait AnyNodeDesc<B: gfx_hal::Backend, T: ?Sized>: std::fmt::Debug {
    /// Find family suitable for the node.
    fn family(&self, families: &[Family<B>]) -> Option<gfx_hal::queue::QueueFamilyId>;

    /// Get buffer resource states.
    fn buffers(&self) -> Vec<chain::BufferState> { Vec::new() }

    /// Get image resource states.
    fn images(&self) -> Vec<chain::ImageState> { Vec::new() }

    /// Build the node.
    fn build<'a>(
        self: Box<Self>,
        factory: &mut Factory<B>,
        aux: &mut T,
        family: gfx_hal::queue::QueueFamilyId,
        buffers: &[NodeBuffer<'a, B>],
        images: &[NodeImage<'a, B>],
    ) -> Result<Box<dyn AnyNode<B, T>>, failure::Error>;

    /// Make node builder.
    fn builder(self) -> NodeBuilder<B, T>
    where
        Self: Sized + 'static,
    {
        NodeBuilder {
            desc: Box::new(self),
            buffers: Vec::new(),
            images: Vec::new(),
            dependencies: Vec::new(),
        }
    }
}

impl<B, T, N> AnyNodeDesc<B, T> for (N,)
where
    B: gfx_hal::Backend,
    T: ?Sized,
    N: NodeDesc<B, T>,
{
    fn family(&self, families: &[Family<B>]) -> Option<gfx_hal::queue::QueueFamilyId> {
        families
            .iter()
            .find(|family| {
                Supports::<<N::Node as Node<B, T>>::Capability>::supports(&family.capability())
                    .is_some()
            }).map(|family| family.index())
    }

    fn buffers(&self) -> Vec<chain::BufferState> {
        N::buffers(&self.0)
    }

    fn images(&self) -> Vec<chain::ImageState> {
        N::images(&self.0)
    }

    fn build<'a>(
        self: Box<Self>,
        factory: &mut Factory<B>,
        aux: &mut T,
        family: gfx_hal::queue::QueueFamilyId,
        buffers: &[NodeBuffer<'a, B>],
        images: &[NodeImage<'a, B>],
    ) -> Result<Box<dyn AnyNode<B, T>>, failure::Error> {
        let node = NodeDesc::build(
            &self.0,
            factory,
            aux,
            family,
            buffers.iter().cloned(),
            images.iter().cloned(),
        )?;
        Ok(Box::new((node,)))
    }
}

/// Builder for the node.
#[derive(derivative::Derivative)]
#[derivative(Debug(bound = ""))]
pub struct NodeBuilder<B: gfx_hal::Backend, T: ?Sized> {
    pub(crate) desc: Box<dyn AnyNodeDesc<B, T>>,
    pub(crate) buffers: Vec<BufferId>,
    pub(crate) images: Vec<ImageId>,
    pub(crate) dependencies: Vec<usize>,
}

impl<B, T> NodeBuilder<B, T>
where
    B: gfx_hal::Backend,
    T: ?Sized,
{
    /// Add buffer to the node.
    /// This method must be called for each buffer node uses.
    pub fn add_buffer(&mut self, buffer: BufferId) -> &mut Self {
        self.buffers.push(buffer);
        self
    }

    /// Add image to the node.
    /// This method must be called for each image node uses.
    pub fn add_image(&mut self, image: ImageId) -> &mut Self {
        self.images.push(image);
        self
    }

    /// Add dependency.
    /// Node will be placed after its dependencies.
    pub fn add_dependency(&mut self, dependency: NodeId) -> &mut Self {
        self.dependencies.push(dependency.0);
        self
    }

    /// Add buffer to the node.
    /// This method must be called for each buffer node uses.
    pub fn with_buffer(mut self, buffer: BufferId) -> Self {
        self.add_buffer(buffer);
        self
    }

    /// Add image to the node.
    /// This method must be called for each image node uses.
    pub fn with_image(mut self, image: ImageId) -> Self {
        self.add_image(image);
        self
    }

    /// Add dependency.
    /// Node will be placed after its dependencies.
    pub fn with_dependency(mut self, dependency: NodeId) -> Self {
        self.add_dependency(dependency);
        self
    }

    pub(crate) fn chain(&self, id: usize, factory: &Factory<B>, buffers: usize) -> chain::Node {
        chain::Node {
            id,
            family: self.desc.family(factory.families()).unwrap(),
            dependencies: self.dependencies.clone(),
            buffers: self
                .buffers
                .iter()
                .map(|id| chain::Id(id.0))
                .zip(self.desc.buffers())
                .collect(),
            images: self
                .images
                .iter()
                .map(|id| chain::Id(id.0 + buffers))
                .zip(self.desc.images())
                .collect(),
        }
    }

    /// Build node from this.
    #[allow(unused)]
    pub(crate) fn build<'a>(
        self,
        factory: &mut Factory<B>,
        aux: &mut T,
        family: gfx_hal::queue::QueueFamilyId,
        buffers: &[Buffer<B>],
        images: &[(Image<B>, Option<gfx_hal::command::ClearValue>)],
        chains: &chain::Chains,
        submission: &chain::Submission<chain::SyncData<usize, usize>>,
    ) -> Result<Box<dyn AnyNode<B, T>>, failure::Error> {
        self.desc.build(
            factory,
            aux,
            family,
            &self.buffers.iter().map(|&BufferId(index)| {
                let id = chain::Id(index);
                NodeBuffer {
                    buffer: &buffers[index],
                    state: chains.buffers[&id].links()[submission.resource_link_index(id)].submission_state(submission.id()),
                }
            }).collect::<Vec<_>>(),
            &self.images.iter().map(|&ImageId(index)| {
                let id = chain::Id(index + buffers.len());
                NodeImage {
                    image: &images[index].0,
                    state: chains.images[&id].links()[submission.resource_link_index(id)].submission_state(submission.id()),
                    clear: if submission.resource_link_index(id) == 0 {
                        images[index].1
                    } else {
                        None
                    }
                }
            }).collect::<Vec<_>>(),
        )
    }
}